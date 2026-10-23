//! New HDF5 files contain /table column datasets and a separate validity group.
//! Fixed names and exact text annotations preserve ambiguous labels and SQL
//! types HDF5 does not natively define, without inventing a null sentinel.
use super::*;
use crate::{adapters::TableSource, export::ExportStage, sessions::OperationContext};
use hdf5_pure::{AttrValue, FileBuilder};
use std::{io::Write, path::PathBuf};

pub struct ExportSource {
    pub object: ObjectDescriptor,
    pub path: PathBuf,
    pub fingerprint: Arc<Fingerprint>,
}
struct Sink<'a> {
    file: File,
    bytes: u64,
    policy: &'a ResourcePolicy,
    context: &'a OperationContext,
}
impl Write for Sink<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.context.check().map_err(std::io::Error::other)?;
        if self.bytes.saturating_add(b.len() as u64) > self.policy.scratch_bytes {
            return Err(std::io::Error::other("HDF5 export exceeds the disk budget"));
        }
        let n = self.file.write(b)?;
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
fn class(cell: &Cell) -> u8 {
    match cell {
        Cell::Null => 0,
        Cell::Number { .. } => 1,
        Cell::Text { .. } => 2,
        Cell::Binary { .. } => 3,
        Cell::Boolean { .. } => 4,
        Cell::Json { .. } => 5,
    }
}
fn text(cell: &Cell) -> &str {
    match cell {
        Cell::Number { value, .. } | Cell::Text { value, .. } | Cell::Json { value, .. } => value,
        Cell::Binary { preview_hex, .. } => preview_hex,
        _ => "",
    }
}
fn complete(cell: &Cell) -> bool {
    match cell {
        Cell::Number { truncated, .. }
        | Cell::Text { truncated, .. }
        | Cell::Json { truncated, .. }
        | Cell::Binary { truncated, .. } => !*truncated,
        _ => true,
    }
}
fn integer_type(signed: bool, size: u32) -> Ty {
    Ty::FixedPoint {
        size,
        byte_order: Order::LittleEndian,
        layout: hdf5_pure::FixedPointLayout {
            signed,
            bit_offset: 0,
            bit_precision: (size * 8) as u16,
        },
    }
}
fn output_error(e: impl std::fmt::Display) -> AppError {
    AppError::new(ErrorCode::Export, format!("Could not write HDF5: {e}"))
}

pub fn export(
    table: &mut dyn TableSource,
    generation: u64,
    options: ExportOptions,
    source: ExportSource,
    policy: &ResourcePolicy,
    context: &OperationContext,
) -> AppResult<ExportResult> {
    context.check()?;
    if options.max_rows == 0 || options.max_rows > policy.max_export_rows {
        return Err(AppError::limit("Choose a bounded export row cap"));
    }
    let stage = ExportStage::new(
        &options.destination,
        ExportFormat::Hdf5,
        Some(&source.path),
        policy,
    )?;
    let mut p = policy.clone();
    p.cell_preview_bytes = p.full_cell_bytes;
    p.blob_preview_bytes = p.full_cell_bytes;
    let mut query = options.view.clone();
    query.offset = 0;
    query.page_size = policy.max_page_size.min(64);
    let mut columns: Vec<ColumnDescriptor> = vec![];
    let mut data: Vec<Vec<Cell>> = vec![];
    let mut bytes = 0usize;
    let mut count = 0u64;
    let truncated;
    loop {
        context.check()?;
        query.page_size = query.page_size.min((options.max_rows - count + 1) as usize);
        let page = match table.fetch_page(generation, &query, &p) {
            Ok(page) => page,
            Err(e) if e.code == ErrorCode::ResourceLimit && query.page_size > 1 => {
                query.page_size = (query.page_size / 2).max(1);
                continue;
            }
            Err(e) => return Err(e),
        };
        if columns.is_empty() {
            columns = page
                .column_ids
                .iter()
                .map(|id| {
                    source
                        .object
                        .columns
                        .iter()
                        .find(|c| c.id == *id)
                        .cloned()
                        .ok_or_else(AppError::internal)
                })
                .collect::<AppResult<Vec<_>>>()?;
            data = vec![vec![]; columns.len()];
        }
        let mut extra = false;
        for row in page.rows {
            if count == options.max_rows {
                extra = true;
                break;
            }
            for (i, cell) in row.into_iter().enumerate() {
                if !complete(&cell) {
                    return Err(AppError::limit("An HDF5 export cell exceeds the full-cell budget; hide it or use another format"));
                }
                bytes = bytes.saturating_add(cell.payload_bytes() + 128);
                if bytes as u64 > policy.hdf5_read_bytes {
                    return Err(AppError::limit(
                        "HDF5 export exceeds the buffer budget; choose fewer rows or columns",
                    ));
                }
                data[i].push(cell);
            }
            count += 1;
        }
        if extra {
            truncated = true;
            break;
        }
        if !page.has_more {
            truncated = false;
            break;
        }
        query.offset = count;
    }
    let mut builder = FileBuilder::new();
    builder.set_attr("sivro.format", AttrValue::String("column-table-v1".into()));
    let mut group = builder.create_group("table");
    group.set_attr("sivro.table", AttrValue::I64(1));
    let mut validity = builder.create_group("_sivro_validity");
    let ambiguous = crate::dataset::ambiguous_labels(&source.object.columns);
    let mut encoded = 0u64;
    for (i, (column, values)) in columns.iter().zip(&data).enumerate() {
        context.check()?;
        let name = format!("col-{i:04}");
        let mask = values
            .iter()
            .map(|v| u8::from(!matches!(v, Cell::Null)))
            .collect::<Vec<_>>();
        validity.create_dataset(&name).with_u8_data(&mask);
        let ds = group.create_dataset(&name);
        let kinds = values
            .iter()
            .map(class)
            .filter(|k| *k != 0)
            .collect::<std::collections::HashSet<_>>();
        if kinds.len() > 1 {
            return Err(output_error(format!(
                "{} mixes cell types; export JSON/JSONL or filter the view",
                column.name
            )));
        }
        let kind = kinds.iter().next().copied().unwrap_or(match column.kind {
            ColumnKind::Number => 1,
            ColumnKind::Bool => 4,
            ColumnKind::Binary => 3,
            ColumnKind::Nested => 5,
            _ => 2,
        });
        let mut ty = column.data_type.clone();
        match kind {
            1 => {
                let real=values.iter().any(|c|matches!(c,Cell::Number{logical_type,..}if matches!(logical_type.as_str(),"FLOAT"|"DOUBLE"|"REAL"|"SQLITE_REAL")))||matches!(ty.as_str(),"FLOAT"|"DOUBLE");
                if real {
                    let nums=values.iter().map(|c|if matches!(c,Cell::Null){Ok(0.0)}else{
                        if let Cell::Number{logical_type,..}=c{if !matches!(logical_type.as_str(),"FLOAT"|"DOUBLE"|"REAL"|"SQLITE_REAL")&&text(c).parse::<i128>().is_ok_and(|n|n.unsigned_abs()>1<<53){return Err(output_error("Mixed real and wide integer values would round in HDF5"));}}
                        text(c).parse::<f64>().map_err(output_error)
                    }).collect::<AppResult<Vec<_>>>()?;
                    ds.with_f64_data(&nums);
                    if !matches!(ty.as_str(), "FLOAT" | "DOUBLE") {
                        ty = "DOUBLE".into();
                    }
                    encoded = encoded.saturating_add(count * 8);
                } else if !ty.starts_with("DECIMAL")
                    && ty != sql::JSON_NUMBER
                    && values
                        .iter()
                        .all(|c| matches!(c, Cell::Null) || text(c).parse::<i128>().is_ok())
                {
                    let signed =
                        values.iter().any(|c| text(c).starts_with('-')) || !ty.starts_with('U');
                    let size = if values.iter().all(|c| {
                        matches!(c, Cell::Null)
                            || if signed {
                                text(c).parse::<i64>().is_ok()
                            } else {
                                text(c).parse::<u64>().is_ok()
                            }
                    }) {
                        8
                    } else {
                        16
                    };
                    let mut raw = vec![];
                    for v in values {
                        let n = if matches!(v, Cell::Null) {
                            0
                        } else {
                            text(v).parse::<i128>().map_err(output_error)?
                        };
                        if size == 8 {
                            raw.extend_from_slice(&(n as i64).to_le_bytes());
                        } else {
                            raw.extend_from_slice(&n.to_le_bytes());
                        }
                    }
                    encoded = encoded.saturating_add(raw.len() as u64);
                    ds.with_raw_data(integer_type(signed, size), raw, count);
                    ty = if signed {
                        if size == 8 {
                            "BIGINT"
                        } else {
                            "HUGEINT"
                        }
                    } else if size == 8 {
                        "UBIGINT"
                    } else {
                        "UHUGEINT"
                    }
                    .into();
                } else if !ty.starts_with("DECIMAL")
                    && ty != sql::JSON_NUMBER
                    && values
                        .iter()
                        .all(|c| matches!(c, Cell::Null) || text(c).parse::<u128>().is_ok())
                {
                    let mut raw = vec![];
                    for v in values {
                        let n = if matches!(v, Cell::Null) {
                            0
                        } else {
                            text(v).parse::<u128>().map_err(output_error)?
                        };
                        raw.extend_from_slice(&n.to_le_bytes());
                    }
                    encoded = encoded.saturating_add(raw.len() as u64);
                    ds.with_raw_data(integer_type(false, 16), raw, count);
                    ty = "UHUGEINT".into();
                } else {
                    let strings = values.iter().map(text).collect::<Vec<_>>();
                    ds.with_vlen_strings(&strings);
                    if logical_type(&ty).is_none() {
                        ty = sql::JSON_NUMBER.into();
                    }
                    encoded = encoded
                        .saturating_add(strings.iter().map(|s| s.len() as u64 + 32).sum::<u64>());
                }
            }
            4 => {
                ds.with_u8_data(
                    &values
                        .iter()
                        .map(|c| u8::from(matches!(c, Cell::Boolean { value: true })))
                        .collect::<Vec<_>>(),
                );
                ty = "BOOLEAN".into();
                encoded = encoded.saturating_add(count);
            }
            _ => {
                let encoded_strings: Option<Vec<String>> =
                    if kind == 2 && values.iter().any(|c| text(c).contains('\0')) {
                        ds.set_attr("sivro.encoding", AttrValue::String("utf8_hex".into()));
                        Some(values.iter().map(|c| hex(text(c).as_bytes())).collect())
                    } else {
                        None
                    };
                let strings = if let Some(ref encoded) = encoded_strings {
                    encoded.iter().map(String::as_str).collect::<Vec<_>>()
                } else {
                    values.iter().map(text).collect::<Vec<_>>()
                };
                ds.with_vlen_strings(&strings);
                ty = if kind == 3 {
                    "BLOB".into()
                } else if kind == 5 {
                    sql::JSON_VALUE.into()
                } else {
                    logical_type(&ty)
                        .filter(|t| {
                            matches!(
                                t.as_str(),
                                "VARCHAR"
                                    | "DATE"
                                    | "TIME"
                                    | "TIMESTAMP"
                                    | "TIMESTAMP_S"
                                    | "TIMESTAMP_MS"
                                    | "TIMESTAMP_NS"
                                    | "TIMESTAMP WITH TIME ZONE"
                            )
                        })
                        .unwrap_or_else(|| "VARCHAR".into())
                };
                encoded = encoded
                    .saturating_add(strings.iter().map(|s| s.len() as u64 + 32).sum::<u64>());
            }
        }
        if encoded > policy.hdf5_read_bytes {
            return Err(AppError::limit(
                "Encoded HDF5 export exceeds the buffer budget",
            ));
        }
        ds.set_attr(
            "sivro.name",
            AttrValue::String(if ambiguous {
                column.id.clone()
            } else {
                column.name.clone()
            }),
        );
        ds.set_attr("sivro.type", AttrValue::String(ty));
        ds.set_attr(
            "sivro.validity",
            AttrValue::String(format!("/_sivro_validity/{name}")),
        );
    }
    builder.add_group(group.finish());
    builder.add_group(validity.finish());
    context.check()?;
    free_space(
        stage.file.parent().ok_or_else(AppError::internal)?,
        policy,
        encoded,
    )?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stage.file)
        .map_err(AppError::io)?;
    let mut sink = Sink {
        file,
        bytes: 0,
        policy,
        context,
    };
    builder.finish_to(&mut sink).map_err(output_error)?;
    sink.flush().map_err(AppError::io)?;
    drop(sink);
    context.publish(|| {
        source.fingerprint.check(&source.path)?;
        stage.publish()
    })?;
    let mut warnings=vec!["HDF5 contains /table column datasets and /_sivro_validity masks. Exact decimal, temporal, binary and nested values use Sivro annotations; source hierarchy and attributes are not reproduced.".into()];
    if ambiguous {
        warnings.push("Duplicate/blank labels: column IDs are used as export labels.".into());
    }
    if truncated {
        warnings.push(
            "The export reached the requested row cap; additional matching rows were omitted."
                .into(),
        );
    }
    Ok(ExportResult {
        session_generation: generation,
        object_id: source.object.id,
        destination: stage.destination.to_string_lossy().into_owned(),
        exported_rows: count.to_string(),
        truncated,
        warnings,
    })
}
