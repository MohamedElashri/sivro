use crate::{
    api::*,
    dataset::*,
    errors::{AppError, AppResult},
    policy::ResourcePolicy,
    query::{duckdb as sql, Query},
    registry::Fingerprint,
    scratch::SessionScratch,
};
use duckdb::{params_from_iter, Connection, Row};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

pub struct DuckDbAdapter {
    pub format: FormatKind,
    pub hdf5: Option<Hdf5Selection>,
    pub arrow: Option<ArrowInfo>,
    pub delimited: Option<super::delimited::FrozenOptions>,
    pub excel: Option<ExcelSelection>,
    pub conn: Connection,
    pub path: PathBuf,
    pub fingerprint: Arc<Fingerprint>,
    pub object: ObjectDescriptor,
    pub row_count: Option<u64>,
    pub row_groups: Vec<RowGroupInfo>,
    pub created_by: Option<String>,
    pub format_version: i32,
    pub counts: HashMap<String, u64>,
    pub profiles: HashMap<String, ColumnProfile>,
    /// JSON/JSONL record tables keep each source record in `RAW_RECORD`.
    pub raw_records: bool,
    // Declared last: connection/files drop before scratch files are removed.
    pub _scratch: SessionScratch,
}

impl DuckDbAdapter {
    pub fn descriptor(
        &self,
        session_id: &str,
        generation: u64,
        policy: &ResourcePolicy,
    ) -> DatasetDescriptor {
        DatasetDescriptor {
            api_version: API_VERSION,
            session_id: session_id.into(),
            generation,
            format: self.format,
            delimited: self.delimited.clone(),
            path: self.path.to_string_lossy().into_owned(),
            file_name: self
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            file_size: self.fingerprint.size.to_string(),
            object: self.object.clone(),
            row_groups: self.row_groups.clone(),
            created_by: self.created_by.clone(),
            format_version: self.format_version,
            policy: policy.clone(),
            document: None,
            excel: self.excel.clone(),
            sqlite: None,
            hdf5: self.hdf5.clone(),
            arrow: self.arrow.clone(),
        }
    }

    pub fn known_count(&self, query: &Query) -> AppResult<CountState> {
        if query.unfiltered() {
            if let Some(count) = self.row_count {
                return Ok(CountState::known(count));
            }
        }
        Ok(self
            .counts
            .get(&query.context_key()?)
            .copied()
            .map(CountState::known)
            .unwrap_or(CountState::Unknown))
    }

    pub fn fetch_page(
        &self,
        generation: u64,
        query: &Query,
        policy: &ResourcePolicy,
    ) -> AppResult<PageResult> {
        let start = Instant::now();
        let compiled = sql::compile(&self.conn, &self.object.columns, query, policy)?;
        if compiled.selected.len().saturating_mul(query.page_size) > policy.max_page_bytes / 128 {
            return Err(AppError::limit(
                "Reduce the page size or number of visible columns",
            ));
        }
        let values = select_cells(
            &compiled.selected,
            policy.cell_preview_bytes,
            policy.blob_preview_bytes,
        );
        let query_sql = format!(
            "SELECT {}::VARCHAR, {values} FROM {} {} {} LIMIT {} OFFSET {}",
            sql::ORDINAL,
            sql::VIEW,
            compiled.predicate,
            compiled.order,
            query.page_size + 1,
            query.offset
        );
        let mut statement = self.conn.prepare(&query_sql).map_err(AppError::query)?;
        let mut records = statement
            .query(params_from_iter(compiled.parameters.iter()))
            .map_err(AppError::query)?;
        let mut rows = Vec::new();
        let mut row_ids = Vec::new();
        let mut bytes = 0;
        let mut has_more = false;
        while let Some(row) = records.next().map_err(AppError::query)? {
            if rows.len() == query.page_size {
                has_more = true;
                break;
            }
            let cells = read_cells(row, &compiled.selected, 1, policy.cell_preview_bytes)?;
            bytes += serde_json::to_vec(&cells)
                .map_err(|_| AppError::internal())?
                .len()
                + 128;
            if bytes > policy.max_page_bytes {
                return Err(AppError::limit(
                    "Page payload exceeds the response budget; reduce visible columns or page size",
                ));
            }
            row_ids.push(row.get::<_, String>(0).map_err(AppError::query)?);
            rows.push(cells);
        }
        Ok(PageResult {
            session_generation: generation,
            object_id: self.object.id.clone(),
            column_ids: compiled.selected.iter().map(|c| c.id.clone()).collect(),
            row_ids,
            rows,
            offset: query.offset,
            limit: query.page_size,
            has_more,
            count: self.known_count(query)?,
            query_ms: start.elapsed().as_millis() as u64,
        })
    }

    pub fn count(
        &mut self,
        generation: u64,
        query: &Query,
        policy: &ResourcePolicy,
    ) -> AppResult<CountResult> {
        let compiled = sql::compile(&self.conn, &self.object.columns, query, policy)?;
        if let CountState::Known { value } = self.known_count(query)? {
            return Ok(CountResult {
                session_generation: generation,
                object_id: self.object.id.clone(),
                count: CountState::Known { value },
            });
        }
        let count: u64 = self
            .conn
            .query_row(
                &format!("SELECT count(*) FROM {} {}", sql::VIEW, compiled.predicate),
                params_from_iter(compiled.parameters.iter()),
                |r| r.get(0),
            )
            .map_err(AppError::query)?;
        if self.counts.len() >= policy.max_cache_entries {
            self.counts.clear();
        }
        self.counts.insert(query.context_key()?, count);
        Ok(CountResult {
            session_generation: generation,
            object_id: self.object.id.clone(),
            count: CountState::known(count),
        })
    }

    pub fn read_row(
        &self,
        generation: u64,
        query: &RowQuery,
        policy: &ResourcePolicy,
    ) -> AppResult<RowResult> {
        let row_id: u64 = query
            .row_id
            .parse()
            .map_err(|_| AppError::invalid("Invalid row ID"))?;
        if self.row_count.is_some_and(|count| row_id >= count) {
            return Err(AppError::invalid("That row is outside the source"));
        }
        let selected = sql::projection(&self.object.columns, &query.column_ids, policy)?;
        let values = select_cells(&selected, policy.full_cell_bytes, policy.full_cell_bytes);
        let cells = self
            .conn
            .query_row(
                &format!(
                    "SELECT {values} FROM {} WHERE {} = ? LIMIT 1",
                    sql::VIEW,
                    sql::ORDINAL
                ),
                [row_id],
                |row| Ok(read_cells(row, &selected, 0, policy.full_cell_bytes)),
            )
            .map_err(AppError::query)??;
        let mut bytes = 0;
        for cell in &cells {
            let truncated = match cell {
                Cell::Text { truncated, .. }
                | Cell::Number { truncated, .. }
                | Cell::Json { truncated, .. }
                | Cell::Binary { truncated, .. } => *truncated,
                _ => false,
            };
            bytes += cell.payload_bytes();
            if truncated || bytes > policy.full_cell_bytes {
                return Err(AppError::limit(
                    "Expanded cells/copy exceed the 1 MiB response limit; use export for full data",
                ));
            }
        }
        let raw_record = if query.raw_record {
            if !self.raw_records {
                return Err(AppError::invalid(
                    "Only JSON/JSONL record tables retain source records",
                ));
            }
            let (raw, length): (String, u64) = self
                .conn
                .query_row(
                    &format!(
                        "SELECT substring({raw}, 1, {}), octet_length(encode({raw})) FROM {} WHERE {} = ?",
                        policy.full_cell_bytes + 1,
                        sql::VIEW,
                        sql::ORDINAL,
                        raw = sql::RAW_RECORD
                    ),
                    [row_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(AppError::query)?;
            bytes += raw.len();
            if length > policy.full_cell_bytes as u64 || bytes > policy.full_cell_bytes {
                return Err(AppError::limit(
                    "This source record exceeds the 1 MiB inspection limit; export the row instead",
                ));
            }
            Some(raw)
        } else {
            None
        };
        Ok(RowResult {
            session_generation: generation,
            object_id: self.object.id.clone(),
            row_id: query.row_id.clone(),
            column_ids: query.column_ids.clone(),
            cells,
            raw_record,
        })
    }

    pub fn profile(
        &mut self,
        generation: u64,
        query: &ProfileQuery,
        policy: &ResourcePolicy,
    ) -> AppResult<ColumnProfile> {
        let compiled = sql::compile(&self.conn, &self.object.columns, &query.context, policy)?;
        let col = sql::column(&self.object.columns, &query.column_id)?.clone();
        let key = format!("{}:{}", col.id, query.context.context_key()?);
        if let Some(profile) = self.profiles.get(&key) {
            return Ok(profile.clone());
        }
        let name = sql::quote_ident(&col.sql_name);
        let from = format!("FROM {} {}", sql::VIEW, compiled.predicate);
        let (null_count, non_null_count, distinct_count): (u64, u64, u64) = self
            .conn
            .query_row(
                &format!(
                    "SELECT count(*) - count({name}), count({name}), count(DISTINCT {name}) {from}"
                ),
                params_from_iter(compiled.parameters.iter()),
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(AppError::query)?;
        let mut value_column = col.clone();
        value_column.sql_name = "v".into();
        let value_fields = select_cells(
            std::slice::from_ref(&value_column),
            policy.cell_preview_bytes,
            policy.blob_preview_bytes,
        );
        let mut extremes = Vec::new();
        let mut warnings = Vec::new();
        if !col.sortable {
            // Ordering raw tokens would compare text rather than numbers.
            extremes = vec![Cell::Null, Cell::Null];
            warnings.push("Min/max are unavailable for raw JSON values. Distinct and top values compare exact source text, so 1.0 and 1 differ.".into());
        }
        for aggregate in ["min", "max"].into_iter().filter(|_| col.sortable) {
            let cells = self
                .conn
                .query_row(
                    &format!("SELECT {value_fields} FROM (SELECT {aggregate}({name}) AS v {from})"),
                    params_from_iter(compiled.parameters.iter()),
                    |r| {
                        Ok(read_cells(
                            r,
                            std::slice::from_ref(&value_column),
                            0,
                            policy.cell_preview_bytes,
                        ))
                    },
                )
                .map_err(AppError::query)??;
            extremes.push(cells.into_iter().next().unwrap_or(Cell::Null));
        }
        let mut stmt = self.conn.prepare(&format!("SELECT {value_fields}, n::VARCHAR FROM (SELECT {name} AS v, count(*) AS n {from} GROUP BY {name} ORDER BY n DESC, v LIMIT 15)")).map_err(AppError::query)?;
        let mut rows = stmt
            .query(params_from_iter(compiled.parameters.iter()))
            .map_err(AppError::query)?;
        let mut top_values = Vec::new();
        while let Some(row) = rows.next().map_err(AppError::query)? {
            let value = read_cells(
                row,
                std::slice::from_ref(&value_column),
                0,
                policy.cell_preview_bytes,
            )?
            .into_iter()
            .next()
            .unwrap_or(Cell::Null);
            top_values.push(TopValue {
                value,
                count: row.get(2).map_err(AppError::query)?,
            });
        }
        let profile = ColumnProfile {
            session_generation: generation,
            object_id: self.object.id.clone(),
            column_id: col.id,
            scope: "current_view".into(),
            null_count: null_count.to_string(),
            non_null_count: non_null_count.to_string(),
            distinct_count: distinct_count.to_string(),
            min: extremes.remove(0),
            max: extremes.remove(0),
            top_values,
            warnings,
        };
        if self.profiles.len() >= policy.max_cache_entries {
            self.profiles.clear();
        }
        self.profiles.insert(key, profile.clone());
        Ok(profile)
    }
}

fn select_cells(columns: &[ColumnDescriptor], text_bytes: usize, binary_bytes: usize) -> String {
    columns
        .iter()
        .map(|c| {
            let value = sql::display(c);
            let length = if c.kind == ColumnKind::Binary {
                format!("octet_length({})", sql::quote_ident(&c.sql_name))
            } else {
                format!("octet_length(encode({value}))")
            };
            let prefix = if c.kind == ColumnKind::Binary {
                binary_bytes * 2
            } else {
                text_bytes
            };
            format!("substring({value}, 1, {prefix}), ({length})::VARCHAR")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn read_cells(
    row: &Row<'_>,
    columns: &[ColumnDescriptor],
    start: usize,
    limit: usize,
) -> AppResult<Vec<Cell>> {
    columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let value: Option<String> = row.get(start + i * 2).map_err(AppError::query)?;
            let length: Option<String> = row.get(start + i * 2 + 1).map_err(AppError::query)?;
            let Some(mut value) = value else {
                return Ok(Cell::Null);
            };
            let length = length.ok_or_else(AppError::internal)?;
            let full: usize = length.parse().map_err(|_| AppError::internal())?;
            if c.kind == ColumnKind::Binary {
                return Ok(Cell::Binary {
                    byte_count: length,
                    truncated: value.len() / 2 < full,
                    preview_hex: value,
                });
            }
            if value.len() > limit {
                let mut end = limit;
                while !value.is_char_boundary(end) {
                    end -= 1;
                }
                value.truncate(end);
            }
            let truncated = value.len() < full;
            Ok(match c.kind {
                ColumnKind::Bool => Cell::Boolean {
                    value: value == "true",
                },
                ColumnKind::Number => Cell::Number {
                    value,
                    logical_type: c.data_type.clone(),
                    byte_length: length,
                    truncated,
                },
                ColumnKind::Nested => Cell::Json {
                    value,
                    byte_length: length,
                    truncated,
                },
                _ => Cell::Text {
                    value,
                    byte_length: length,
                    truncated,
                },
            })
        })
        .collect()
}

/// SQL quoting does not disable DuckDB's file globbing. Escape every pattern
/// character in the canonical filename, including its parent directories.
pub fn literal_source_pattern(path: &Path) -> AppResult<String> {
    let text = path
        .to_str()
        .ok_or_else(|| AppError::invalid("This path cannot be represented as Unicode"))?;
    #[cfg(windows)]
    let text = {
        // Rust canonical paths use the extended prefix. Its '?' is a Windows
        // namespace marker, not a filename character or a DuckDB wildcard.
        let normalized = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            format!("//{unc}")
        } else {
            text.strip_prefix(r"\\?\").unwrap_or(text).to_owned()
        }
        .replace('\\', "/");
        // Refuse names whose meaning changes without the extended prefix
        // (e.g. trailing dots) instead of reading a different Windows file.
        if !same_file::is_same_file(path, &normalized).map_err(AppError::io)? {
            return Err(AppError::invalid(
                "This Windows path cannot be read unambiguously",
            ));
        }
        normalized
    };
    #[cfg(unix)]
    if text.contains('\\') && text.chars().any(|c| matches!(c, '*' | '?' | '[')) {
        // DuckDB treats backslashes as separators during glob traversal even
        // on Unix. Such mixed literal names cannot be expressed safely.
        return Err(AppError::invalid(
            "A path containing both backslashes and glob characters is unsupported",
        ));
    }
    let mut pattern = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '*' => pattern.push_str("[*]"),
            '?' => pattern.push_str("[?]"),
            '[' => pattern.push_str("[[]"),
            _ => pattern.push(ch),
        }
    }
    Ok(pattern)
}

pub fn connection(scratch: &SessionScratch, policy: &ResourcePolicy) -> AppResult<Connection> {
    let conn = Connection::open_in_memory().map_err(AppError::query)?;
    let temp = scratch
        .path
        .to_str()
        .ok_or_else(|| AppError::invalid("Invalid scratch path"))?;
    conn.execute_batch(&format!(
        "SET autoinstall_known_extensions=false; SET autoload_known_extensions=false;
        SET threads=1; SET preserve_insertion_order=true; SET memory_limit='{}B';
        SET max_temp_directory_size='{}B'; SET temp_directory={};",
        policy.duckdb_memory_bytes,
        policy.scratch_bytes,
        sql::quote_literal(temp)
    ))
    .map_err(AppError::query)?;
    Ok(conn)
}

/// Detection/opening also runs native work under the shared soft deadline.
/// The caller retains the connection until the timer has joined.
pub fn with_deadline<T>(
    conn: &Connection,
    policy: &ResourcePolicy,
    work: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    let (done, receiver) = std::sync::mpsc::channel::<()>();
    let interrupt = conn.interrupt_handle();
    let budget = std::time::Duration::from_millis(policy.timeout_ms);
    let started = Instant::now();
    let monitor = std::thread::Builder::new()
        .name("sivro-open-deadline".into())
        .spawn(move || {
            if matches!(
                receiver.recv_timeout(budget),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                interrupt.interrupt();
            }
        })
        .map_err(AppError::io)?;
    let result = work();
    let _ = done.send(());
    let _ = monitor.join();
    if started.elapsed() >= budget {
        Err(AppError::new(crate::errors::ErrorCode::Timeout, "Parsing preview exceeded its time budget. Choose explicit parsing settings or a smaller input."))
    } else {
        result
    }
}
