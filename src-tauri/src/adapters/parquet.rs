use super::duckdb::{connection, literal_source_pattern, DuckDbAdapter};
use crate::{
    dataset::*,
    errors::{AppError, AppResult, ErrorCode},
    policy::ResourcePolicy,
    query::duckdb as sql,
    registry::Fingerprint,
    scratch::SessionScratch,
};
use parquet::file::reader::{FileReader, SerializedFileReader};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

pub struct ParquetAdapter;
impl ParquetAdapter {
    pub fn open(
        path: &Path,
        object_id: String,
        scratch: SessionScratch,
        policy: &ResourcePolicy,
    ) -> AppResult<DuckDbAdapter> {
        let fingerprint = Arc::new(Fingerprint::read(path)?);
        let mut file = std::fs::File::open(path).map_err(AppError::io)?;
        // Bound the footer buffer before the metadata reader allocates it.
        let mut trailer = [0u8; 8];
        file.seek(SeekFrom::End(-8)).map_err(AppError::io)?;
        file.read_exact(&mut trailer).map_err(AppError::io)?;
        let footer_bytes =
            u32::from_le_bytes(trailer[..4].try_into().map_err(|_| AppError::internal())?) as u64;
        if footer_bytes + 8 > fingerprint.size {
            return Err(AppError::new(
                ErrorCode::FormatMismatch,
                "The Parquet footer length is invalid",
            ));
        }
        if footer_bytes > policy.max_page_bytes as u64 {
            return Err(AppError::limit(
                "Parquet footer metadata exceeds the metadata budget",
            ));
        }
        file.seek(SeekFrom::Start(0)).map_err(AppError::io)?;
        let reader = SerializedFileReader::new(file).map_err(|e| {
            AppError::new(
                ErrorCode::FormatMismatch,
                format!("Not a valid Parquet file: {e}"),
            )
        })?;
        let metadata = reader.metadata();
        let meta = metadata.file_metadata();
        if meta.num_rows() < 0 || metadata.num_row_groups() > policy.max_page_bytes / 128 {
            return Err(AppError::limit(
                "Parquet metadata exceeds the supported resource budget",
            ));
        }
        let row_count = meta.num_rows() as u64;
        let row_groups = metadata
            .row_groups()
            .iter()
            .enumerate()
            .map(|(index, rg)| RowGroupInfo {
                index,
                num_rows: rg.num_rows().to_string(),
                bytes: rg.total_byte_size().to_string(),
                num_columns: rg.num_columns(),
            })
            .collect();
        let created_by = meta.created_by().map(str::to_string);
        let format_version = meta.version();
        let conn = connection(&scratch, policy)?;
        let source_pattern = literal_source_pattern(path)?;
        let source = format!("read_parquet({})", sql::quote_literal(&source_pattern));
        let mut stmt = conn
            .prepare(&format!("DESCRIBE SELECT * FROM {source}"))
            .map_err(AppError::query)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(AppError::query)?;
        let mut columns = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let (name, data_type, nullable) = row.map_err(AppError::query)?;
            bytes += name.len() + data_type.len() + 128;
            if columns.len() >= policy.max_columns || bytes > policy.max_page_bytes {
                return Err(AppError::limit(
                    "Parquet schema exceeds the metadata budget",
                ));
            }
            let kind = sql::classify_kind(&data_type);
            columns.push(ColumnDescriptor {
                id: format!("col-{}", columns.len()),
                sql_name: format!("field_{}", columns.len()),
                name,
                data_type,
                kind,
                nullable: nullable.eq_ignore_ascii_case("YES"),
                operators: sql::operators(kind),
                sortable: true,
            });
        }
        drop(stmt);
        if columns.is_empty() {
            return Err(AppError::invalid("This file has no columns"));
        }
        let projection = columns
            .iter()
            .map(|c| {
                format!(
                    "{} AS {}",
                    sql::quote_ident(&c.name),
                    sql::quote_ident(&c.sql_name)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        // A single-threaded insertion-order scan assigns a source ordinal before
        // filtering/sorting. It also disambiguates identical sort keys and names
        // that collide with generated aliases. No source column is a row key.
        conn.execute_batch(&format!("CREATE TEMP VIEW {} AS SELECT {projection}, row_number() OVER () - 1 AS {} FROM {source}", sql::VIEW, sql::ORDINAL)).map_err(AppError::query)?;
        fingerprint.check(path)?;
        let object = ObjectDescriptor { id: object_id, label: "Data".into(), kind: ObjectKind::Table, columns,
            row_count: CountState::known(row_count), capabilities: Capabilities { page: true, filter: true, sort: true, profile: true, exact_count: true, copy: true, exports: vec!["parquet".into(), "csv".into(), "tsv".into(), "json".into(), "jsonl".into()] },
            warnings: vec!["Paging uses a source ordinal from a single-threaded, insertion-order scan; refreshing invalidates all row identities.".into(), "Structured comparisons are limited to text matching and NULL checks to avoid lossy JSON numeric casts.".into()] };
        Ok(DuckDbAdapter {
            format: FormatKind::Parquet,
            delimited: None,
            excel: None,
            conn,
            path: path.to_path_buf(),
            fingerprint,
            object,
            row_count: Some(row_count),
            row_groups,
            created_by,
            format_version,
            counts: HashMap::new(),
            profiles: HashMap::new(),
            raw_records: false,
            _scratch: scratch,
        })
    }
}
