//! Arrow IPC files (including Feather V2) and streams, staged without touching
//! the source. Preflight bounds metadata, bodies and decompression before the
//! IPC reader allocates them. DuckDB receives batches through its native bridge.
use super::duckdb::{connection, with_deadline, DuckDbAdapter};
use crate::{
    api::*,
    dataset::*,
    errors::{AppError, AppResult, ErrorCode},
    policy::ResourcePolicy,
    query::duckdb as sql,
    registry::Fingerprint,
    scratch::{free_space, SessionScratch},
};
use arrow_array::{Array, ArrayRef, RecordBatch, RecordBatchReader, StringArray};
use arrow_cast::{
    cast,
    display::{ArrayFormatter, FormatOptions},
};
use arrow_ipc::reader::{FileReader, StreamReader};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use duckdb::vtab::arrow::{arrow_recordbatch_to_query_params, ArrowVTab};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
    time::Instant,
};

fn invalid(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::FormatMismatch,
        format!("Invalid Arrow IPC data: {error}"),
    )
}
fn deadline(start: Instant, policy: &ResourcePolicy) -> AppResult<()> {
    if start.elapsed().as_millis() > policy.timeout_ms as u128 {
        return Err(AppError::new(
            ErrorCode::Timeout,
            "Opening Arrow exceeded the time budget",
        ));
    }
    Ok(())
}

/// Types that the native bridge can represent without truncation. Other
/// logical types remain readable as text, with their original types reported.
fn native_type(ty: &DataType) -> Option<DataType> {
    use DataType::*;
    match ty {
        Null => Some(Utf8),
        Float16 => Some(Float32),
        Dictionary(_, value) => native_type(value),
        Boolean | Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 | Float32
        | Float64 | Utf8 | LargeUtf8 | Utf8View | Binary | LargeBinary | BinaryView
        | FixedSizeBinary(_) | Date32 | Time32(_) => Some(ty.clone()),
        Timestamp(TimeUnit::Nanosecond, Some(_)) | Time64(TimeUnit::Nanosecond) => None,
        Timestamp(_, _) | Time64(TimeUnit::Microsecond) => Some(ty.clone()),
        Decimal32(_, s) | Decimal64(_, s) | Decimal128(_, s) if *s >= 0 => Some(ty.clone()),
        List(f) => Some(List(Arc::new(native_field(f)?))),
        LargeList(f) => Some(LargeList(Arc::new(native_field(f)?))),
        FixedSizeList(f, n) if *n > 0 => Some(FixedSizeList(Arc::new(native_field(f)?), *n)),
        Struct(fields) if !fields.is_empty() => {
            let mut names = std::collections::HashSet::new();
            if fields
                .iter()
                .any(|f| f.name().is_empty() || !names.insert(f.name().to_ascii_lowercase()))
            {
                return None;
            }
            Some(Struct(
                fields
                    .iter()
                    .map(|f| native_field(f).map(Arc::new))
                    .collect::<Option<Vec<_>>>()?
                    .into(),
            ))
        }
        Map(f, sorted) => Some(Map(Arc::new(native_field(f)?), *sorted)),
        _ => None,
    }
}
fn nullable(field: &Field) -> bool {
    field.is_nullable()
        || matches!(
            field.data_type(),
            DataType::Null
                | DataType::Dictionary(_, _)
                | DataType::RunEndEncoded(_, _)
                | DataType::Union(_, _)
        )
}

fn native_field(field: &Field) -> Option<Field> {
    Some(Field::new(
        field.name(),
        native_type(field.data_type())?,
        nullable(field),
    ))
}

fn normalize(
    batch: &RecordBatch,
    schema: &Arc<Schema>,
    policy: &ResourcePolicy,
) -> AppResult<RecordBatch> {
    let arrays = batch
        .columns()
        .iter()
        .zip(schema.fields())
        .map(|(array, field)| {
            if field.data_type() == &DataType::Utf8 && native_type(array.data_type()).is_none() {
                let options = FormatOptions::default().with_display_error(false);
                let formatter =
                    ArrayFormatter::try_new(array.as_ref(), &options).map_err(invalid)?;
                let nulls = array.logical_nulls();
                let mut bytes = 0usize;
                let values = (0..array.len())
                    .map(|i| {
                        if nulls.as_ref().is_some_and(|n| n.is_null(i)) {
                            return Ok(None);
                        }
                        let mut text = String::new();
                        formatter.value(i).write(&mut text).map_err(invalid)?;
                        bytes = bytes.saturating_add(text.len());
                        if bytes > policy.arrow_batch_bytes as usize {
                            return Err(AppError::limit(
                                "Arrow text conversion exceeds the batch budget",
                            ));
                        }
                        Ok(Some(text))
                    })
                    .collect::<AppResult<Vec<_>>>()?;
                Ok(Arc::new(StringArray::from(values)) as ArrayRef)
            } else {
                cast(array, field.data_type()).map_err(invalid)
            }
        })
        .collect::<AppResult<Vec<_>>>()?;
    RecordBatch::try_new(schema.clone(), arrays).map_err(invalid)
}

fn check_scratch(scratch: &SessionScratch, policy: &ResourcePolicy) -> AppResult<()> {
    let bytes: u64 = std::fs::read_dir(&scratch.path)
        .map_err(AppError::io)?
        .map(|entry| {
            entry
                .and_then(|entry| entry.metadata())
                .map(|meta| meta.len())
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(AppError::io)?
        .into_iter()
        .sum();
    if bytes > policy.scratch_bytes {
        return Err(AppError::limit("Arrow staging exceeded the scratch budget"));
    }
    free_space(&scratch.path, policy, 0)
}

pub fn open(
    path: &Path,
    object_id: String,
    scratch: SessionScratch,
    policy: &ResourcePolicy,
) -> AppResult<DuckDbAdapter> {
    let started = Instant::now();
    let fingerprint = Arc::new(Fingerprint::read(path)?);
    if fingerprint.size > policy.arrow_file_bytes {
        return Err(AppError::limit(
            "Arrow input exceeds the file budget (256 MiB by default)",
        ));
    }
    let mut file = File::open(path).map_err(AppError::io)?;
    let is_file = preflight(&mut file, fingerprint.size, policy, started)?;
    file.rewind().map_err(AppError::io)?;
    let reader: Box<dyn RecordBatchReader> = if is_file {
        Box::new(FileReader::try_new(file, None).map_err(invalid)?)
    } else {
        Box::new(StreamReader::try_new(file, None).map_err(invalid)?)
    };
    let original = reader.schema();
    if original.fields().is_empty() {
        return Err(AppError::invalid("This Arrow input has no columns"));
    }
    if original.fields().len() > policy.max_columns
        || format!("{original:?}").len() > policy.max_page_bytes
    {
        return Err(AppError::limit("Arrow schema exceeds the metadata budget"));
    }
    let mut warnings = vec![format!("Arrow IPC {}: all record batches validated and staged locally. Dictionary encoding and original schema/field metadata are not retained in exports.", if is_file { "file / Feather V2" } else { "stream" })];
    let fields = original.fields().iter().enumerate().map(|(i, field)| {
        let ty = native_type(field.data_type()).unwrap_or_else(|| {
            warnings.push(format!("{}: {} is shown/exported as text to preserve its precision; numeric/temporal comparisons do not apply.", field.name(), field.data_type()));
            DataType::Utf8
        });
        Field::new(format!("field_{i}"), ty, nullable(field))
    }).collect::<Vec<_>>();
    let schema = Arc::new(Schema::new(fields));
    let conn = connection(&scratch, policy)?;
    let mut row_count = 0u64;
    with_deadline(&conn, policy, || {
        conn.execute_batch(&format!(
            "ATTACH {} AS sivro_arrow",
            sql::quote_literal(
                scratch
                    .path
                    .join("arrow.duckdb")
                    .to_str()
                    .ok_or_else(AppError::internal)?
            )
        ))
        .map_err(AppError::query)?;
        conn.register_table_function::<ArrowVTab>("sivro_arrow_batch")
            .map_err(AppError::query)?;
        conn.execute(
            "CREATE TABLE sivro_arrow.records AS SELECT * FROM sivro_arrow_batch(?, ?)",
            arrow_recordbatch_to_query_params(RecordBatch::new_empty(schema.clone())),
        )
        .map_err(AppError::query)?;
        check_scratch(&scratch, policy)?;
        for batch in reader {
            deadline(started, policy)?;
            let batch = batch.map_err(invalid)?;
            if batch.get_array_memory_size() as u64 > policy.arrow_batch_bytes {
                return Err(AppError::limit(
                    "Arrow record batch exceeds the decoded batch budget",
                ));
            }
            let batch = normalize(&batch, &schema, policy)?;
            row_count = row_count
                .checked_add(batch.num_rows() as u64)
                .ok_or_else(AppError::internal)?;
            conn.execute(
                "INSERT INTO sivro_arrow.records SELECT * FROM sivro_arrow_batch(?, ?)",
                arrow_recordbatch_to_query_params(batch),
            )
            .map_err(AppError::query)?;
            check_scratch(&scratch, policy)?;
        }
        conn.execute_batch(&format!("CREATE TEMP VIEW {} AS SELECT *, row_number() OVER () - 1 AS {} FROM sivro_arrow.records", sql::VIEW, sql::ORDINAL)).map_err(AppError::query)?;
        Ok(())
    })?;
    let mut statement = conn
        .prepare("DESCRIBE SELECT * FROM sivro_arrow.records")
        .map_err(AppError::query)?;
    let types = statement
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(AppError::query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(AppError::query)?;
    drop(statement);
    let columns = original
        .fields()
        .iter()
        .zip(types)
        .enumerate()
        .map(|(i, (f, ty))| super::stage::column(i, f.name().clone(), ty, nullable(f)))
        .collect();
    fingerprint.check(path)?;
    deadline(started, policy)?;
    Ok(DuckDbAdapter {
        arrow: Some(ArrowInfo {
            encoding: if is_file { "file" } else { "stream" }.into(),
            metadata: original
                .metadata()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            fields: original
                .fields()
                .iter()
                .map(|f| ArrowFieldInfo {
                    name: f.name().clone(),
                    data_type: f.data_type().to_string(),
                    nullable: f.is_nullable(),
                    metadata: f
                        .metadata()
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                })
                .collect(),
        }),
        format: FormatKind::Arrow,
        delimited: None,
        excel: None,
        conn,
        path: path.to_path_buf(),
        fingerprint,
        object: ObjectDescriptor {
            id: object_id,
            label: "Data".into(),
            kind: ObjectKind::Table,
            columns,
            row_count: CountState::known(row_count),
            capabilities: Capabilities {
                page: true,
                filter: true,
                sort: true,
                profile: true,
                exact_count: true,
                copy: true,
                exports: vec![
                    "parquet".into(),
                    "arrow".into(),
                    "arrow_stream".into(),
                    "csv".into(),
                    "tsv".into(),
                    "json".into(),
                    "jsonl".into(),
                ],
            },
            warnings,
        },
        row_count: Some(row_count),
        row_groups: vec![],
        created_by: None,
        format_version: 0,
        counts: HashMap::new(),
        profiles: HashMap::new(),
        raw_records: false,
        _scratch: scratch,
    })
}

/// Validate framed messages and advertised decompressed buffer sizes before
/// FileReader/StreamReader can allocate. A malformed file never becomes a tab.
fn preflight(
    file: &mut File,
    size: u64,
    policy: &ResourcePolicy,
    started: Instant,
) -> AppResult<bool> {
    let mut magic = [0u8; 6];
    file.read_exact(&mut magic).map_err(invalid)?;
    let is_file = &magic == b"ARROW1";
    let mut decoded = 0u64;
    if is_file {
        if size < 18 {
            return Err(invalid("truncated file footer"));
        }
        file.seek(SeekFrom::End(-10)).map_err(invalid)?;
        let mut trailer = [0u8; 10];
        file.read_exact(&mut trailer).map_err(invalid)?;
        if &trailer[4..] != b"ARROW1" {
            return Err(invalid("missing file footer"));
        }
        let length = i32::from_le_bytes(trailer[..4].try_into().unwrap());
        if length < 0 || length as usize > policy.max_page_bytes || length as u64 + 18 > size {
            return Err(AppError::limit(
                "Arrow footer exceeds the metadata budget or file bounds",
            ));
        }
        let footer_start = size - 10 - length as u64;
        file.seek(SeekFrom::Start(footer_start)).map_err(invalid)?;
        let mut footer = vec![0u8; length as usize];
        file.read_exact(&mut footer).map_err(invalid)?;
        let footer = arrow_ipc::root_as_footer(&footer).map_err(invalid)?;
        for block in footer
            .dictionaries()
            .into_iter()
            .flatten()
            .chain(footer.recordBatches().into_iter().flatten())
        {
            deadline(started, policy)?;
            let start = u64::try_from(block.offset()).map_err(invalid)?;
            let metadata = u64::try_from(block.metaDataLength()).map_err(invalid)?;
            let body = u64::try_from(block.bodyLength()).map_err(invalid)?;
            let end = start
                .checked_add(metadata)
                .and_then(|v| v.checked_add(body))
                .ok_or_else(|| invalid("block length overflow"))?;
            if start < 8 || end > footer_start {
                return Err(invalid("record block is outside the file"));
            }
            file.seek(SeekFrom::Start(start)).map_err(invalid)?;
            let bounds = message(file, footer_start, policy, &mut decoded)?
                .ok_or_else(|| invalid("unexpected end marker in file"))?;
            if bounds != (metadata, body) {
                return Err(invalid("footer block lengths disagree with the message"));
            }
        }
    } else {
        file.rewind().map_err(invalid)?;
        let mut first = true;
        while file.stream_position().map_err(invalid)? < size {
            deadline(started, policy)?;
            if message(file, size, policy, &mut decoded)?.is_none() {
                if first {
                    return Err(invalid("stream has no schema"));
                }
                if file.stream_position().map_err(invalid)? != size {
                    return Err(invalid("trailing bytes after the stream end marker"));
                }
                break;
            }
            first = false;
        }
    }
    Ok(is_file)
}

fn message(
    file: &mut File,
    end: u64,
    policy: &ResourcePolicy,
    total: &mut u64,
) -> AppResult<Option<(u64, u64)>> {
    let start = file.stream_position().map_err(invalid)?;
    let mut prefix = [0u8; 4];
    file.read_exact(&mut prefix).map_err(invalid)?;
    let mut framing = 4;
    if prefix == [255; 4] {
        file.read_exact(&mut prefix).map_err(invalid)?;
        framing = 8;
    }
    let length = i32::from_le_bytes(prefix);
    if length == 0 {
        return Ok(None);
    }
    if length < 0 || length as usize > policy.max_page_bytes {
        return Err(AppError::limit("Arrow message exceeds the metadata budget"));
    }
    let body_start = start
        .checked_add(framing + length as u64)
        .ok_or_else(|| invalid("message overflow"))?;
    if body_start > end {
        return Err(invalid("truncated metadata"));
    }
    let mut bytes = vec![0u8; length as usize];
    file.read_exact(&mut bytes).map_err(invalid)?;
    let message = arrow_ipc::root_as_message(&bytes).map_err(invalid)?;
    let body = u64::try_from(message.bodyLength()).map_err(invalid)?;
    let body_end = body_start
        .checked_add(body)
        .filter(|n| *n <= end)
        .ok_or_else(|| invalid("truncated body"))?;
    if body > policy.arrow_batch_bytes {
        return Err(AppError::limit(
            "Arrow message body exceeds the batch budget",
        ));
    }
    let batch = message
        .header_as_record_batch()
        .or_else(|| message.header_as_dictionary_batch().and_then(|d| d.data()));
    let mut decoded = body;
    if let Some(batch) = batch {
        if batch.length() < 0 || batch.length() as u64 > policy.arrow_batch_bytes / 32 {
            return Err(AppError::limit(
                "Arrow batch row count exceeds the resource budget",
            ));
        }
        if let Some(nodes) = batch.nodes() {
            if nodes
                .iter()
                .any(|n| n.length() < 0 || n.length() as u64 > policy.arrow_batch_bytes / 32)
            {
                return Err(AppError::limit(
                    "Arrow array length exceeds the decoded batch budget",
                ));
            }
        }
        if let Some(buffers) = batch.buffers() {
            decoded = 0;
            for buffer in buffers {
                let offset = u64::try_from(buffer.offset()).map_err(invalid)?;
                let len = u64::try_from(buffer.length()).map_err(invalid)?;
                if offset.checked_add(len).is_none_or(|n| n > body) {
                    return Err(invalid("buffer outside the message body"));
                }
                let expanded = if batch.compression().is_some() && len > 0 {
                    if len < 8 {
                        return Err(invalid("truncated compressed buffer"));
                    }
                    file.seek(SeekFrom::Start(body_start + offset))
                        .map_err(invalid)?;
                    let mut prefix = [0u8; 8];
                    file.read_exact(&mut prefix).map_err(invalid)?;
                    let expected = i64::from_le_bytes(prefix);
                    match expected {
                        -1 => len - 8,
                        n if n >= 0 => {
                            if n as u64 > policy.arrow_batch_bytes {
                                return Err(AppError::limit(
                                    "Compressed Arrow buffer exceeds the decoded budget",
                                ));
                            }
                            // Arrow's LZ4 decoder uses read_to_end; validate the
                            // actual output with a bounded reader first, since a
                            // hostile prefix can understate the expanded length.
                            if batch
                                .compression()
                                .is_some_and(|c| c.codec() == arrow_ipc::CompressionType::LZ4_FRAME)
                            {
                                let mut decoder =
                                    lz4_flex::frame::FrameDecoder::new((&mut *file).take(len - 8))
                                        .take(n as u64 + 1);
                                let mut buffer = [0u8; 8192];
                                let mut found = 0u64;
                                loop {
                                    let count = decoder.read(&mut buffer).map_err(invalid)?;
                                    if count == 0 {
                                        break;
                                    }
                                    found += count as u64;
                                }
                                if found != n as u64 {
                                    return Err(invalid(
                                        "compressed buffer length disagrees with its prefix",
                                    ));
                                }
                            }
                            n as u64
                        }
                        _ => return Err(invalid("negative decompressed buffer length")),
                    }
                } else {
                    len
                };
                decoded = decoded
                    .checked_add(expanded)
                    .ok_or_else(|| invalid("decoded size overflow"))?;
            }
        }
    }
    *total = total
        .checked_add(decoded)
        .ok_or_else(|| invalid("decoded size overflow"))?;
    if decoded > policy.arrow_batch_bytes || *total > policy.scratch_bytes {
        return Err(AppError::limit(
            "Decoded Arrow data exceeds the batch or total budget",
        ));
    }
    file.seek(SeekFrom::Start(body_end)).map_err(invalid)?;
    Ok(Some((framing + length as u64, body)))
}
