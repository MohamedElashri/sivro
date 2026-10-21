//! SQLite through rusqlite: a strictly read-only connection, one selected
//! user table or view, and a SQLite-specific query compiler. Cells keep their
//! per-row storage class; nothing is copied into another engine.
use crate::{
    api::*,
    dataset::*,
    errors::{AppError, AppResult, ErrorCode},
    export::{validate_json, ExportStage},
    policy::ResourcePolicy,
    query::{sqlite as sql, Query},
    registry::Fingerprint,
    sessions::OperationContext,
};
use rusqlite::{
    hooks::{AuthAction, AuthContext, Authorization},
    params_from_iter,
    types::{Value, ValueRef},
    Connection, OpenFlags, Row,
};
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

const KEY: &str = "\"__sivro_key\"";

fn query_error(error: rusqlite::Error) -> AppError {
    use rusqlite::ErrorCode as Code;
    match error.sqlite_error_code() {
        Some(Code::OperationInterrupted) => {
            AppError::new(ErrorCode::Cancelled, "The SQLite query was interrupted")
        }
        Some(Code::DatabaseBusy | Code::DatabaseLocked) => AppError::new(
            ErrorCode::Busy,
            "The database is locked by another connection; retry when its writer finishes",
        ),
        Some(Code::ReadOnly | Code::CannotOpen) => AppError::new(
            ErrorCode::Io,
            format!("SQLite could not read this database read-only: {error}. A WAL-mode database in a read-only folder needs its -wal/-shm files; Sivro does not use immutable mode for live databases."),
        ),
        Some(Code::NotADatabase) => AppError::new(
            ErrorCode::FormatMismatch,
            "This file is not a readable SQLite database (it may be encrypted or damaged)",
        ),
        _ => AppError::query(error),
    }
}

/// Defense in depth beyond read-only flags and `query_only`: generated SQL
/// may only read the main database, call functions and run named pragmas.
fn authorize(context: AuthContext<'_>) -> Authorization {
    match context.action {
        // The bundled engine is built with extension loading available; it
        // stays disabled at runtime, and these functions are refused outright
        // even when a view definition calls them.
        AuthAction::Function { function_name }
            if function_name.eq_ignore_ascii_case("load_extension")
                || function_name.eq_ignore_ascii_case("fts3_tokenizer") =>
        {
            Authorization::Deny
        }
        AuthAction::Select | AuthAction::Recursive | AuthAction::Function { .. } => {
            Authorization::Allow
        }
        AuthAction::Read { .. } if matches!(context.database_name, Some("main") | None) => {
            Authorization::Allow
        }
        AuthAction::Pragma {
            pragma_name:
                "table_list" | "table_xinfo" | "schema_version" | "data_version" | "journal_mode",
            ..
        } => Authorization::Allow,
        _ => Authorization::Deny,
    }
}

#[cfg(test)]
pub(crate) fn connect_for_test(path: &Path) -> AppResult<Connection> {
    connect(path, &ResourcePolicy::default())
}

fn connect(path: &Path, policy: &ResourcePolicy) -> AppResult<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(query_error)?;
    conn.busy_timeout(Duration::from_millis(policy.timeout_ms.min(2000)))
        .map_err(query_error)?;
    conn.execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF;")
        .map_err(query_error)?;
    conn.authorizer(Some(authorize)).map_err(query_error)?;
    Ok(conn)
}

fn pragma_i64(conn: &Connection, name: &str) -> AppResult<i64> {
    conn.query_row(&format!("PRAGMA main.{name}"), [], |r| r.get(0))
        .map_err(query_error)
}

fn sidecars(path: &Path) -> [bool; 2] {
    ["-wal", "-shm"].map(|suffix| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name).exists()
    })
}

fn database_info(conn: &Connection) -> AppResult<DatabaseInfo> {
    let version: String = conn
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .map_err(query_error)?;
    let journal: String = conn
        .query_row("PRAGMA main.journal_mode", [], |r| r.get(0))
        .map_err(query_error)?;
    let mut stmt = conn
        .prepare("PRAGMA main.table_list")
        .map_err(query_error)?;
    let mut rows = stmt.query([]).map_err(query_error)?;
    let mut objects = Vec::new();
    while let Some(row) = rows.next().map_err(query_error)? {
        let name: String = row.get(1).map_err(query_error)?;
        let kind: String = row.get(2).map_err(query_error)?;
        // Shadow tables back virtual tables; sqlite_* objects are internal.
        if name.to_ascii_lowercase().starts_with("sqlite_") || kind == "shadow" {
            continue;
        }
        let kind = match kind.as_str() {
            "table" => DbObjectKind::Table,
            "view" => DbObjectKind::View,
            "virtual" => DbObjectKind::Virtual,
            _ => continue,
        };
        let reason = (kind == DbObjectKind::Virtual)
            .then(|| "Virtual tables run extension modules; Sivro does not open them.".to_string());
        objects.push(DbObjectInfo {
            name,
            kind,
            columns: row.get(3).map_err(query_error)?,
            without_rowid: row.get::<_, i64>(4).map_err(query_error)? != 0,
            selectable: reason.is_none(),
            reason,
        });
        if objects.len() > 100_000 {
            return Err(AppError::limit("The database lists too many objects"));
        }
    }
    objects.sort_by(|a, b| a.name.cmp(&b.name));
    let mut warnings = vec!["Read-only: Sivro never writes, attaches or runs free-form SQL. Pages are read live, so another writer's commits appear on the next page.".to_string()];
    if journal.eq_ignore_ascii_case("wal") {
        warnings.push("WAL mode: SQLite readers need the -wal/-shm files beside the database and may create them; database content is never changed.".into());
    }
    Ok(DatabaseInfo {
        sqlite_version: version,
        journal_mode: journal,
        objects,
        warnings,
    })
}

pub fn inspect(path: &Path, policy: &ResourcePolicy) -> AppResult<DatabaseInfo> {
    let fingerprint = Fingerprint::read_identity(path)?;
    let conn = connect(path, policy)?;
    let info = database_info(&conn)?;
    fingerprint.check(path)?;
    Ok(info)
}

/// How rows are ordered and re-found for expansion.
enum Key {
    /// The rowid under its first unshadowed alias.
    Rowid,
    /// WITHOUT ROWID: primary-key columns in key order (column indices).
    Primary(Vec<usize>),
    /// Views and tables whose rowid aliases are all shadowed: a position from
    /// `row_number()`, which is not stable across changes.
    Position,
}

pub struct SqliteAdapter {
    pub conn: Connection,
    pub path: PathBuf,
    pub fingerprint: Arc<Fingerprint>,
    object: ObjectDescriptor,
    selection: SqliteSelection,
    source: String,
    key: Key,
    schema_version: i64,
    data_version: i64,
    counts: HashMap<String, u64>,
    profiles: HashMap<String, ColumnProfile>,
}

pub fn open(
    path: &Path,
    options: &SqliteOptions,
    object_id: String,
    policy: &ResourcePolicy,
) -> AppResult<SqliteAdapter> {
    let fingerprint = Arc::new(Fingerprint::read_identity(path)?);
    let before = sidecars(path);
    let conn = connect(path, policy)?;
    let database = database_info(&conn)?;
    let object = database
        .objects
        .iter()
        .find(|o| o.name == options.object)
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::InvalidObject,
                "That table or view is not in this database. Choose one again.",
            )
        })?;
    if let Some(reason) = &object.reason {
        return Err(AppError::new(ErrorCode::InvalidObject, reason.clone()));
    }
    let (kind, without_rowid) = (object.kind, object.without_rowid);
    // (name, declared type, primary-key position); vtab-hidden columns
    // (hidden=1) are not part of `SELECT *`.
    let mut info: Vec<(String, String, i64)> = Vec::new();
    {
        let mut stmt = conn
            .prepare("SELECT cid, name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo(?, 'main')")
            .map_err(query_error)?;
        let mut rows = stmt.query([&options.object]).map_err(query_error)?;
        while let Some(row) = rows.next().map_err(query_error)? {
            if row.get::<_, i64>(6).map_err(query_error)? == 1 {
                continue;
            }
            info.push((
                row.get(1).map_err(query_error)?,
                row.get::<_, Option<String>>(2)
                    .map_err(query_error)?
                    .unwrap_or_default(),
                row.get(5).map_err(query_error)?,
            ));
            if info.len() > policy.max_columns {
                return Err(AppError::limit(
                    "The object has more columns than the column budget",
                ));
            }
        }
    }
    if info.is_empty() {
        return Err(AppError::invalid("This object has no readable columns"));
    }
    let names: Vec<String> = (0..info.len()).map(|i| format!("c{i}")).collect();
    let target = format!("main.{}", sql::quote_ident(&options.object));
    let alias = ["rowid", "oid", "_rowid_"]
        .into_iter()
        .find(|a| !info.iter().any(|(n, _, _)| n.eq_ignore_ascii_case(a)));
    let key = if kind == DbObjectKind::Table && without_rowid {
        let mut pk: Vec<(i64, usize)> = info
            .iter()
            .enumerate()
            .filter(|(_, c)| c.2 > 0)
            .map(|(i, c)| (c.2, i))
            .collect();
        pk.sort();
        Key::Primary(pk.into_iter().map(|(_, i)| i).collect())
    } else if kind == DbObjectKind::Table && alias.is_some() {
        Key::Rowid
    } else {
        Key::Position
    };
    let list = names.join(", ");
    // CTE column lists rename positionally, so duplicate view labels stay
    // addressable and no source name is reused in generated SQL.
    let source = match &key {
        Key::Rowid => format!(
            "WITH {}({KEY}, {list}) AS (SELECT {}, * FROM {target})",
            sql::SOURCE,
            alias.unwrap_or("rowid")
        ),
        Key::Primary(_) => format!("WITH {}({list}) AS (SELECT * FROM {target})", sql::SOURCE),
        Key::Position => format!(
            "WITH sivro_base({list}) AS (SELECT * FROM {target}), {} AS (SELECT row_number() OVER () AS {KEY}, * FROM sivro_base)",
            sql::SOURCE
        ),
    };
    let expected = names.len() + usize::from(!matches!(key, Key::Primary(_)));
    let actual = conn
        .prepare(&format!("{source} SELECT * FROM {} LIMIT 0", sql::SOURCE))
        .map_err(query_error)?
        .column_count();
    if actual != expected {
        return Err(AppError::invalid(
            "The object's columns did not match its declared schema",
        ));
    }
    let columns: Vec<ColumnDescriptor> = info
        .iter()
        .zip(&names)
        .enumerate()
        .map(|(i, ((name, declared, _), sql_name))| {
            let kind = sql::kind(declared);
            ColumnDescriptor {
                id: format!("col-{i}"),
                name: name.clone(),
                sql_name: sql_name.clone(),
                data_type: if declared.is_empty() {
                    "ANY".into()
                } else {
                    declared.clone()
                },
                kind,
                nullable: true,
                operators: sql::operators(kind),
                sortable: true,
            }
        })
        .collect();
    let mut warnings = vec![
        "SQLite is dynamically typed: each cell shows its own storage class (integer, real, text or blob), whatever the declared type. Sorting follows SQLite order: NULL, numbers, text, then blobs.".to_string(),
        "Text matching uses SQLite LIKE, which ignores case only for ASCII letters.".into(),
    ];
    warnings.push(match &key {
        Key::Rowid => "Rows are ordered by rowid, which also breaks sort ties.".into(),
        Key::Primary(_) => "Rows are ordered by the primary key, which also breaks sort ties.".into(),
        Key::Position if kind == DbObjectKind::View => "Views have no stable row key: rows are numbered in SQLite's scan order, so paging and expanded rows can shift if the database changes.".into(),
        Key::Position => "Columns named rowid, oid and _rowid_ hide the row key: rows are numbered in scan order and can shift if the database changes.".into(),
    });
    let after = sidecars(path);
    if before.iter().zip(after).any(|(b, a)| !b && a) {
        warnings.push("Opening this WAL-mode database created SQLite's -wal/-shm files beside it, as SQLite requires; the database content was not changed.".into());
    }
    let schema_version = pragma_i64(&conn, "schema_version")?;
    let data_version = pragma_i64(&conn, "data_version")?;
    fingerprint.check(path)?;
    let selection = SqliteSelection {
        object: options.object.clone(),
        database,
    };
    Ok(SqliteAdapter {
        conn,
        path: path.to_path_buf(),
        fingerprint,
        object: ObjectDescriptor {
            id: object_id,
            label: options.object.clone(),
            kind: ObjectKind::Table,
            columns,
            row_count: CountState::Unknown,
            capabilities: Capabilities {
                page: true,
                filter: true,
                sort: true,
                profile: true,
                exact_count: true,
                copy: true,
                exports: ["parquet", "csv", "tsv", "json", "jsonl"]
                    .map(String::from)
                    .to_vec(),
            },
            warnings,
        },
        selection,
        source,
        key,
        schema_version,
        data_version,
        counts: HashMap::new(),
        profiles: HashMap::new(),
    })
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// Shortest round-trip text for a REAL; JSON has no non-finite numbers.
fn real_text(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        format!("{f:?}")
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Selects a bounded preview: text and blobs are cut in SQL before they reach
/// Rust, with their full byte length alongside. `substr` turns an empty blob
/// into NULL, so empty blobs pass through unchanged.
fn bounded(name: &str, text_chars: usize, blob_bytes: usize) -> String {
    format!(
        "CASE typeof({name}) WHEN 'text' THEN substr({name}, 1, {text_chars}) WHEN 'blob' THEN CASE length({name}) WHEN 0 THEN {name} ELSE substr({name}, 1, {blob_bytes}) END ELSE {name} END, CASE typeof({name}) WHEN 'text' THEN length(CAST({name} AS BLOB)) WHEN 'blob' THEN length({name}) END"
    )
}

fn cell(row: &Row<'_>, at: usize, text_limit: usize) -> AppResult<Cell> {
    let length: Option<i64> = row.get(at + 1).map_err(query_error)?;
    let full = length.unwrap_or(0).max(0) as usize;
    Ok(match row.get_ref(at).map_err(query_error)? {
        ValueRef::Null => Cell::Null,
        ValueRef::Integer(i) => {
            let value = i.to_string();
            Cell::Number {
                byte_length: value.len().to_string(),
                value,
                logical_type: "INTEGER".into(),
                truncated: false,
            }
        }
        ValueRef::Real(f) => {
            let value = real_text(f);
            Cell::Number {
                byte_length: value.len().to_string(),
                value,
                logical_type: "REAL".into(),
                truncated: false,
            }
        }
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => {
                let mut end = text.len().min(text_limit);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                Cell::Text {
                    value: text[..end].into(),
                    byte_length: full.to_string(),
                    truncated: end < full,
                }
            }
            // Text that is not UTF-8 is shown as bytes rather than decoded lossily.
            Err(_) => Cell::Binary {
                byte_count: full.to_string(),
                preview_hex: hex(&bytes[..bytes.len().min(text_limit)]),
                truncated: bytes.len() < full,
            },
        },
        ValueRef::Blob(bytes) => Cell::Binary {
            byte_count: full.to_string(),
            preview_hex: hex(bytes),
            truncated: bytes.len() < full,
        },
    })
}

/// Row IDs for primary keys: tagged exact values, so read_row can bind them
/// with their original storage class.
fn encode_key(row: &Row<'_>, columns: &[usize], start: usize) -> AppResult<String> {
    let parts = columns
        .iter()
        .enumerate()
        .map(|(i, _)| {
            Ok(match row.get_ref(start + i).map_err(query_error)? {
                ValueRef::Null => "n".to_string(),
                ValueRef::Integer(v) => format!("i:{v}"),
                ValueRef::Real(v) => format!("r:{:016x}", v.to_bits()),
                ValueRef::Text(v) => format!("t:{}", hex(v)),
                ValueRef::Blob(v) => format!("b:{}", hex(v)),
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    serde_json::to_string(&parts).map_err(|_| AppError::internal())
}
fn decode_key(id: &str) -> AppResult<Vec<Value>> {
    let bad = || AppError::invalid("Invalid row ID");
    let unhex = |s: &str| -> AppResult<Vec<u8>> {
        if !s.len().is_multiple_of(2) {
            return Err(bad());
        }
        (0..s.len() / 2)
            .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| bad()))
            .collect()
    };
    let parts: Vec<String> = serde_json::from_str(id).map_err(|_| bad())?;
    parts
        .iter()
        .map(|p| {
            Ok(match p.split_at_checked(2).unwrap_or((p, "")) {
                ("n", _) if p == "n" => Value::Null,
                ("i:", v) => Value::Integer(v.parse().map_err(|_| bad())?),
                ("r:", v) => Value::Real(f64::from_bits(
                    u64::from_str_radix(v, 16).map_err(|_| bad())?,
                )),
                ("t:", v) => Value::Text(String::from_utf8(unhex(v)?).map_err(|_| bad())?),
                ("b:", v) => Value::Blob(unhex(v)?),
                _ => return Err(bad()),
            })
        })
        .collect()
}

impl SqliteAdapter {
    fn key_order(&self) -> String {
        match &self.key {
            Key::Primary(cols) => cols
                .iter()
                .map(|i| sql::quote_ident(&format!("c{i}")))
                .collect::<Vec<_>>()
                .join(", "),
            _ => KEY.into(),
        }
    }
    fn key_columns(&self) -> String {
        match &self.key {
            Key::Primary(cols) => cols
                .iter()
                .map(|i| sql::quote_ident(&format!("c{i}")))
                .collect::<Vec<_>>()
                .join(", "),
            _ => KEY.into(),
        }
    }
    fn key_width(&self) -> usize {
        match &self.key {
            Key::Primary(cols) => cols.len(),
            _ => 1,
        }
    }
    fn row_id(&self, row: &Row<'_>) -> AppResult<String> {
        match &self.key {
            Key::Primary(cols) => encode_key(row, cols, 0),
            _ => Ok(row.get::<_, i64>(0).map_err(query_error)?.to_string()),
        }
    }

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
            format: FormatKind::Sqlite,
            delimited: None,
            path: self.path.to_string_lossy().into_owned(),
            file_name: self
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            file_size: self.fingerprint.size.to_string(),
            object: self.object.clone(),
            row_groups: vec![],
            created_by: None,
            format_version: 0,
            policy: policy.clone(),
            document: None,
            excel: None,
            sqlite: Some(self.selection.clone()),
        }
    }

    /// Replacement and schema changes make the session stale; committed data
    /// changes are live and only invalidate cached counts and profiles.
    pub fn check_source(&mut self) -> AppResult<()> {
        self.fingerprint.check(&self.path)?;
        if pragma_i64(&self.conn, "schema_version")? != self.schema_version {
            return Err(AppError::new(
                ErrorCode::StaleDataset,
                "The database schema changed. Refresh to reopen it.",
            ));
        }
        let data_version = pragma_i64(&self.conn, "data_version")?;
        if data_version != self.data_version {
            self.data_version = data_version;
            self.counts.clear();
            self.profiles.clear();
        }
        Ok(())
    }

    fn compile(&self, query: &Query, policy: &ResourcePolicy) -> AppResult<sql::Compiled> {
        sql::compile(&self.object.columns, query, &self.key_order(), policy)
    }

    fn known_count(&self, query: &Query) -> AppResult<CountState> {
        Ok(self
            .counts
            .get(&query.context_key()?)
            .copied()
            .map(CountState::known)
            .unwrap_or(CountState::Unknown))
    }

    fn count_rows(&self, compiled: &sql::Compiled, limit: Option<u64>) -> AppResult<u64> {
        let limit = limit.map_or(String::new(), |n| format!(" LIMIT {n}"));
        self.conn
            .query_row(
                &format!(
                    "{} SELECT count(*) FROM (SELECT 1 FROM {} {}{limit})",
                    self.source,
                    sql::SOURCE,
                    compiled.predicate
                ),
                params_from_iter(compiled.params.iter()),
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n as u64)
            .map_err(query_error)
    }
}

impl super::TableSource for SqliteAdapter {
    fn fetch_page(
        &self,
        generation: u64,
        query: &Query,
        policy: &ResourcePolicy,
    ) -> AppResult<PageResult> {
        let started = Instant::now();
        let compiled = self.compile(query, policy)?;
        if compiled.selected.len().saturating_mul(query.page_size) > policy.max_page_bytes / 128 {
            return Err(AppError::limit(
                "Reduce the page size or number of visible columns",
            ));
        }
        let values = compiled
            .selected
            .iter()
            .map(|c| {
                bounded(
                    &sql::quote_ident(&c.sql_name),
                    policy.cell_preview_bytes,
                    policy.blob_preview_bytes,
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut params = compiled.params.clone();
        params.push(Value::Integer(query.page_size as i64 + 1));
        params.push(Value::Integer(query.offset as i64));
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{} SELECT {}, {values} FROM {} {} {} LIMIT ? OFFSET ?",
                self.source,
                self.key_columns(),
                sql::SOURCE,
                compiled.predicate,
                compiled.order
            ))
            .map_err(query_error)?;
        let mut rows = stmt
            .query(params_from_iter(params.iter()))
            .map_err(query_error)?;
        let (mut out, mut ids, mut bytes, mut has_more) = (Vec::new(), Vec::new(), 0usize, false);
        let width = self.key_width();
        while let Some(row) = rows.next().map_err(query_error)? {
            if out.len() == query.page_size {
                has_more = true;
                break;
            }
            let cells = (0..compiled.selected.len())
                .map(|i| cell(row, width + i * 2, policy.cell_preview_bytes))
                .collect::<AppResult<Vec<_>>>()?;
            bytes += cells.iter().map(Cell::payload_bytes).sum::<usize>() + 128;
            if bytes > policy.max_page_bytes {
                return Err(AppError::limit(
                    "Page payload exceeds the response budget; reduce visible columns or page size",
                ));
            }
            ids.push(self.row_id(row)?);
            out.push(cells);
        }
        Ok(PageResult {
            session_generation: generation,
            object_id: self.object.id.clone(),
            column_ids: compiled.selected.iter().map(|c| c.id.clone()).collect(),
            row_ids: ids,
            rows: out,
            offset: query.offset,
            limit: query.page_size,
            has_more,
            count: self.known_count(query)?,
            query_ms: started.elapsed().as_millis() as u64,
        })
    }

    fn count(
        &mut self,
        generation: u64,
        query: &Query,
        policy: &ResourcePolicy,
    ) -> AppResult<CountResult> {
        let compiled = self.compile(query, policy)?;
        let count = match self.known_count(query)? {
            CountState::Known { value } => value.parse().map_err(|_| AppError::internal())?,
            _ => self.count_rows(&compiled, None)?,
        };
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

    fn profile(
        &mut self,
        generation: u64,
        query: &ProfileQuery,
        policy: &ResourcePolicy,
    ) -> AppResult<ColumnProfile> {
        let compiled = self.compile(&query.context, policy)?;
        let col = crate::query::duckdb::column(&self.object.columns, &query.column_id)?.clone();
        let key = format!("{}:{}", col.id, query.context.context_key()?);
        if let Some(profile) = self.profiles.get(&key) {
            return Ok(profile.clone());
        }
        let name = sql::quote_ident(&col.sql_name);
        let from = format!("FROM {} {}", sql::SOURCE, compiled.predicate);
        let params = || params_from_iter(compiled.params.iter());
        let (nulls, filled, distinct): (i64, i64, i64) = self
            .conn
            .query_row(
                &format!("{} SELECT count(*) - count({name}), count({name}), count(DISTINCT {name}) {from}", self.source),
                params(),
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(query_error)?;
        let value = bounded("v", policy.cell_preview_bytes, policy.blob_preview_bytes);
        let mut extremes = Vec::new();
        for aggregate in ["min", "max"] {
            extremes.push(
                self.conn
                    .query_row(
                        &format!(
                            "{} SELECT {value} FROM (SELECT {aggregate}({name}) AS v {from})",
                            self.source
                        ),
                        params(),
                        |r| Ok(cell(r, 0, policy.cell_preview_bytes)),
                    )
                    .map_err(query_error)??,
            );
        }
        let mut top_values = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare(&format!("{} SELECT {value}, n FROM (SELECT {name} AS v, count(*) AS n {from} GROUP BY {name} ORDER BY n DESC, v LIMIT 15)", self.source))
                .map_err(query_error)?;
            let mut rows = stmt.query(params()).map_err(query_error)?;
            while let Some(row) = rows.next().map_err(query_error)? {
                top_values.push(TopValue {
                    value: cell(row, 0, policy.cell_preview_bytes)?,
                    count: row.get::<_, i64>(2).map_err(query_error)?.to_string(),
                });
            }
        }
        let mut classes = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare(&format!(
                    "{} SELECT typeof({name}), count(*) {from} GROUP BY 1 ORDER BY 1",
                    self.source
                ))
                .map_err(query_error)?;
            let mut rows = stmt.query(params()).map_err(query_error)?;
            while let Some(row) = rows.next().map_err(query_error)? {
                classes.push(format!(
                    "{} {}",
                    row.get::<_, String>(0).map_err(query_error)?,
                    row.get::<_, i64>(1).map_err(query_error)?
                ));
            }
        }
        let profile = ColumnProfile {
            session_generation: generation,
            object_id: self.object.id.clone(),
            column_id: col.id,
            scope: "current_view".into(),
            null_count: nulls.to_string(),
            non_null_count: filled.to_string(),
            distinct_count: distinct.to_string(),
            max: extremes.pop().unwrap_or(Cell::Null),
            min: extremes.pop().unwrap_or(Cell::Null),
            top_values,
            warnings: vec![format!("Storage classes in this view: {}. Min/max follow SQLite order across classes; 1 and 1.0 count as the same value.", classes.join(", "))],
        };
        if self.profiles.len() >= policy.max_cache_entries {
            self.profiles.clear();
        }
        self.profiles.insert(key, profile.clone());
        Ok(profile)
    }

    fn read_row(
        &self,
        generation: u64,
        query: &RowQuery,
        policy: &ResourcePolicy,
    ) -> AppResult<RowResult> {
        if query.raw_record {
            return Err(AppError::invalid(
                "Only JSON/JSONL record tables retain source records",
            ));
        }
        let selected =
            crate::query::duckdb::projection(&self.object.columns, &query.column_ids, policy)?;
        let (predicate, params) = match &self.key {
            Key::Primary(cols) => {
                let values = decode_key(&query.row_id)?;
                if values.len() != cols.len() {
                    return Err(AppError::invalid("Invalid row ID"));
                }
                let terms = cols
                    .iter()
                    .map(|i| format!("{} IS ?", sql::quote_ident(&format!("c{i}"))))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                (terms, values)
            }
            _ => (
                format!("{KEY} = ?"),
                vec![Value::Integer(
                    query
                        .row_id
                        .parse()
                        .map_err(|_| AppError::invalid("Invalid row ID"))?,
                )],
            ),
        };
        let values = selected
            .iter()
            .map(|c| {
                bounded(
                    &sql::quote_ident(&c.sql_name),
                    policy.full_cell_bytes + 1,
                    policy.full_cell_bytes + 1,
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let cells = self
            .conn
            .query_row(
                &format!(
                    "{} SELECT {values} FROM {} WHERE {predicate} LIMIT 1",
                    self.source,
                    sql::SOURCE
                ),
                params_from_iter(params.iter()),
                |row| {
                    Ok((0..selected.len())
                        .map(|i| cell(row, i * 2, policy.full_cell_bytes))
                        .collect::<AppResult<Vec<_>>>())
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::new(
                    ErrorCode::InvalidInput,
                    "That row no longer exists in the database",
                ),
                other => query_error(other),
            })??;
        let mut bytes = 0;
        for cell in &cells {
            bytes += cell.payload_bytes();
            let truncated = matches!(
                cell,
                Cell::Text {
                    truncated: true,
                    ..
                } | Cell::Binary {
                    truncated: true,
                    ..
                }
            );
            if truncated || bytes > policy.full_cell_bytes {
                return Err(AppError::limit(
                    "Expanded cells/copy exceed the 1 MiB response limit; use export for full data",
                ));
            }
        }
        Ok(RowResult {
            session_generation: generation,
            object_id: self.object.id.clone(),
            row_id: query.row_id.clone(),
            column_ids: query.column_ids.clone(),
            cells,
            raw_record: None,
        })
    }

    fn export(
        &mut self,
        generation: u64,
        options: ExportOptions,
        policy: &ResourcePolicy,
        context: &OperationContext,
    ) -> AppResult<ExportResult> {
        context.check()?;
        if options.max_rows == 0 || options.max_rows > policy.max_export_rows {
            return Err(AppError::limit(format!(
                "Export row cap must be between 1 and {}",
                policy.max_export_rows
            )));
        }
        let compiled = self.compile(&options.view, policy)?;
        let stage = ExportStage::new(
            &options.destination,
            options.format,
            Some(&self.path),
            policy,
        )?;
        let found = self.count_rows(&compiled, Some(options.max_rows + 1))?;
        context.check()?;
        let ambiguous = ambiguous_labels(&self.object.columns);
        let labels: Vec<&str> = compiled
            .selected
            .iter()
            .map(|c| {
                if ambiguous {
                    c.id.as_str()
                } else {
                    c.name.as_str()
                }
            })
            .collect();
        let select = format!(
            "{} SELECT {} FROM {} {} {} LIMIT {}",
            self.source,
            compiled
                .selected
                .iter()
                .map(|c| sql::quote_ident(&c.sql_name))
                .collect::<Vec<_>>()
                .join(", "),
            sql::SOURCE,
            compiled.predicate,
            compiled.order,
            options.max_rows
        );
        let job = Export {
            conn: &self.conn,
            select: &select,
            params: &compiled.params,
            file: &stage.file,
            policy,
            context,
        };
        let (written, binary) = match options.format {
            ExportFormat::Csv => job.delimited(&labels, b',')?,
            ExportFormat::Tsv => job.delimited(&labels, b'\t')?,
            ExportFormat::Json => job.json(&labels, true)?,
            ExportFormat::Jsonl => job.json(&labels, false)?,
            ExportFormat::Parquet => job.parquet(&labels)?,
        };
        if matches!(options.format, ExportFormat::Json | ExportFormat::Jsonl) {
            validate_json(&stage.file, options.format == ExportFormat::Json, context)?;
        }
        context.publish(|| {
            self.fingerprint.check(&self.path)?;
            stage.publish()
        })?;
        let mut warnings = vec![
            "Rows were read live; commits by another writer during the export can be included."
                .to_string(),
        ];
        if ambiguous {
            warnings
                .push("Duplicate/blank labels: column IDs are used as export keys/headers.".into());
        }
        if binary {
            warnings.push(
                match options.format {
                    ExportFormat::Csv | ExportFormat::Tsv => {
                        "Binary cells use hex:-prefixed text in CSV/TSV."
                    }
                    ExportFormat::Json | ExportFormat::Jsonl => {
                        "Binary cells use tagged hexadecimal objects in JSON."
                    }
                    ExportFormat::Parquet => "Binary cells are Parquet BINARY values.",
                }
                .into(),
            );
        }
        if found > options.max_rows {
            warnings.push(
                "The export reached the requested row cap; additional matching rows were omitted."
                    .into(),
            );
        }
        Ok(ExportResult {
            session_generation: generation,
            object_id: self.object.id.clone(),
            destination: stage.destination.to_string_lossy().into_owned(),
            exported_rows: written.to_string(),
            truncated: found > options.max_rows,
            warnings,
        })
    }
}

// ---------------------------------------------------------------------------
// Streaming exports
// ---------------------------------------------------------------------------

struct Export<'a> {
    conn: &'a Connection,
    select: &'a str,
    params: &'a [Value],
    file: &'a Path,
    policy: &'a ResourcePolicy,
    context: &'a OperationContext,
}

impl Export<'_> {
    /// Visits every exported row, checking cancellation and staging size.
    fn rows(&self, mut each: impl FnMut(&Row<'_>) -> AppResult<()>) -> AppResult<u64> {
        let mut stmt = self.conn.prepare(self.select).map_err(query_error)?;
        let mut rows = stmt
            .query(params_from_iter(self.params.iter()))
            .map_err(query_error)?;
        let mut count = 0u64;
        while let Some(row) = rows.next().map_err(query_error)? {
            each(row)?;
            count += 1;
            if count.is_multiple_of(1024) {
                self.context.check()?;
            }
        }
        Ok(count)
    }
    fn writer(&self) -> AppResult<std::io::BufWriter<std::fs::File>> {
        Ok(std::io::BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.file)
                .map_err(AppError::io)?,
        ))
    }
    fn limit(&self, out: &mut std::io::BufWriter<std::fs::File>, rows: u64) -> AppResult<()> {
        if rows.is_multiple_of(1024) {
            out.flush().map_err(AppError::io)?;
            if self.file.metadata().map_err(AppError::io)?.len() > self.policy.scratch_bytes {
                return Err(AppError::limit("Export staging exceeded the disk budget"));
            }
            crate::scratch::free_space(self.file.parent().unwrap_or(self.file), self.policy, 0)?;
        }
        Ok(())
    }

    fn delimited(&self, labels: &[&str], delimiter: u8) -> AppResult<(u64, bool)> {
        let mut out = self.writer()?;
        let quote = |field: &str, line: &mut Vec<u8>| {
            if field.is_empty()
                || field
                    .bytes()
                    .any(|b| b == delimiter || matches!(b, b'"' | b'\r' | b'\n'))
            {
                line.push(b'"');
                line.extend(field.replace('"', "\"\"").bytes());
                line.push(b'"');
            } else {
                line.extend(field.bytes());
            }
        };
        let mut line = Vec::new();
        for (i, label) in labels.iter().enumerate() {
            if i > 0 {
                line.push(delimiter);
            }
            quote(label, &mut line);
        }
        line.push(b'\n');
        out.write_all(&line).map_err(AppError::io)?;
        let (mut binary, mut written) = (false, 0u64);
        self.rows(|row| {
            line.clear();
            for i in 0..labels.len() {
                if i > 0 {
                    line.push(delimiter);
                }
                match row.get_ref(i).map_err(query_error)? {
                    ValueRef::Null => line.extend(br"\N"),
                    ValueRef::Integer(v) => line.extend(v.to_string().bytes()),
                    ValueRef::Real(v) => line.extend(real_text(v).bytes()),
                    ValueRef::Text(v) => match std::str::from_utf8(v) {
                        Ok(r"\N") => return Err(AppError::new(ErrorCode::Export, "A value equals the CSV/TSV NULL marker \\N. Choose Parquet or JSON to avoid ambiguity.")),
                        Ok(text) => quote(text, &mut line),
                        Err(_) => {
                            binary = true;
                            line.extend(format!("hex:{}", hex(v)).bytes());
                        }
                    },
                    ValueRef::Blob(v) => {
                        binary = true;
                        line.extend(format!("hex:{}", hex(v)).bytes());
                    }
                }
            }
            line.push(b'\n');
            out.write_all(&line).map_err(AppError::io)?;
            written += 1;
            self.limit(&mut out, written)
        })?;
        out.flush().map_err(AppError::io)?;
        Ok((written, binary))
    }

    fn json(&self, labels: &[&str], array: bool) -> AppResult<(u64, bool)> {
        let keys = labels
            .iter()
            .map(|k| serde_json::to_string(k).map_err(|_| AppError::internal()))
            .collect::<AppResult<Vec<_>>>()?;
        let mut out = self.writer()?;
        if array {
            out.write_all(b"[").map_err(AppError::io)?;
        }
        let (mut binary, mut written, mut line) = (false, 0u64, Vec::new());
        self.rows(|row| {
            line.clear();
            if array {
                line.extend(if written > 0 { &b",\n"[..] } else { &b"\n"[..] });
            }
            line.push(b'{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    line.push(b',');
                }
                line.extend(key.bytes());
                line.push(b':');
                let tagged = |bytes: &[u8]| {
                    format!(
                        "{{\"$binary_hex\":\"{}\",\"byteCount\":\"{}\"}}",
                        hex(bytes),
                        bytes.len()
                    )
                };
                match row.get_ref(i).map_err(query_error)? {
                    ValueRef::Null => line.extend(b"null"),
                    ValueRef::Integer(v) => line.extend(v.to_string().bytes()),
                    // Non-finite values fail the validation pass below.
                    ValueRef::Real(v) => line.extend(real_text(v).bytes()),
                    ValueRef::Text(v) => match std::str::from_utf8(v) {
                        Ok(text) => serde_json::to_writer(&mut line, text)
                            .map_err(|_| AppError::internal())?,
                        Err(_) => {
                            binary = true;
                            line.extend(tagged(v).bytes());
                        }
                    },
                    ValueRef::Blob(v) => {
                        binary = true;
                        line.extend(tagged(v).bytes());
                    }
                }
            }
            line.push(b'}');
            if !array {
                line.push(b'\n');
            }
            out.write_all(&line).map_err(AppError::io)?;
            written += 1;
            self.limit(&mut out, written)
        })?;
        if array {
            out.write_all(if written > 0 { b"\n]\n" } else { b"]\n" })
                .map_err(AppError::io)?;
        }
        out.flush().map_err(AppError::io)?;
        Ok((written, binary))
    }

    /// Parquet needs one type per column: a first pass records each column's
    /// storage classes; mixed classes fail instead of being coerced.
    fn parquet(&self, labels: &[&str]) -> AppResult<(u64, bool)> {
        use arrow_array::{builder::*, ArrayRef, RecordBatch};
        use arrow_schema::{DataType, Field, Schema};
        #[derive(Clone, Copy, Default)]
        struct Seen {
            int: bool,
            real: bool,
            text: bool,
            blob: bool,
            wide: bool,
        }
        let mut seen = vec![Seen::default(); labels.len()];
        self.rows(|row| {
            for (i, s) in seen.iter_mut().enumerate() {
                match row.get_ref(i).map_err(query_error)? {
                    ValueRef::Null => {}
                    ValueRef::Integer(v) => {
                        s.int = true;
                        s.wide |= v.unsigned_abs() > 1 << 53;
                    }
                    ValueRef::Real(_) => s.real = true,
                    ValueRef::Text(v) if std::str::from_utf8(v).is_ok() => s.text = true,
                    ValueRef::Text(_) | ValueRef::Blob(_) => s.blob = true,
                }
            }
            Ok(())
        })?;
        let types = seen
            .iter()
            .zip(labels)
            .map(|(s, label)| {
                let classes = usize::from(s.int || s.real) + usize::from(s.text) + usize::from(s.blob);
                if classes > 1 {
                    return Err(AppError::new(ErrorCode::Export, format!("{label} mixes numbers, text and/or blobs; Parquet needs one type per column. Export CSV, TSV, JSON or JSONL, or filter the view.")));
                }
                Ok(if s.real && s.int && s.wide {
                    return Err(AppError::new(ErrorCode::Export, format!("{label} mixes REAL values with integers beyond 2^53, which a Parquet DOUBLE column would round.")));
                } else if s.real {
                    DataType::Float64
                } else if s.int {
                    DataType::Int64
                } else if s.blob {
                    DataType::Binary
                } else {
                    DataType::Utf8
                })
            })
            .collect::<AppResult<Vec<_>>>()?;
        let schema = Arc::new(Schema::new(
            labels
                .iter()
                .zip(&types)
                .map(|(l, t)| Field::new(*l, t.clone(), true))
                .collect::<Vec<_>>(),
        ));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.file)
            .map_err(AppError::io)?;
        let mut writer =
            parquet::arrow::ArrowWriter::try_new(file, schema.clone(), None).map_err(|e| {
                AppError::new(ErrorCode::Export, format!("Could not write Parquet: {e}"))
            })?;
        let mut builders: Vec<Box<dyn ArrayBuilder>> = types
            .iter()
            .map(|t| -> Box<dyn ArrayBuilder> {
                match t {
                    DataType::Int64 => Box::new(Int64Builder::new()),
                    DataType::Float64 => Box::new(Float64Builder::new()),
                    DataType::Binary => Box::new(BinaryBuilder::new()),
                    _ => Box::new(StringBuilder::new()),
                }
            })
            .collect();
        let mut pending = 0usize;
        let flush = |builders: &mut Vec<Box<dyn ArrayBuilder>>,
                     writer: &mut parquet::arrow::ArrowWriter<std::fs::File>|
         -> AppResult<()> {
            let arrays: Vec<ArrayRef> = builders.iter_mut().map(|b| b.finish()).collect();
            let batch = RecordBatch::try_new(schema.clone(), arrays).map_err(|e| {
                AppError::new(
                    ErrorCode::Export,
                    format!("Could not build Parquet rows: {e}"),
                )
            })?;
            writer.write(&batch).map_err(|e| {
                AppError::new(ErrorCode::Export, format!("Could not write Parquet: {e}"))
            })
        };
        let mut binary = false;
        let written = self.rows(|row| {
            for (i, (builder, ty)) in builders.iter_mut().zip(&types).enumerate() {
                let value = row.get_ref(i).map_err(query_error)?;
                let any = builder.as_any_mut();
                match ty {
                    DataType::Int64 => {
                        let b = any
                            .downcast_mut::<Int64Builder>()
                            .ok_or_else(AppError::internal)?;
                        match value {
                            ValueRef::Integer(v) => b.append_value(v),
                            _ => b.append_null(),
                        }
                    }
                    DataType::Float64 => {
                        let b = any
                            .downcast_mut::<Float64Builder>()
                            .ok_or_else(AppError::internal)?;
                        match value {
                            ValueRef::Real(v) => b.append_value(v),
                            // Checked above: every integer here fits a double exactly.
                            ValueRef::Integer(v) => b.append_value(v as f64),
                            _ => b.append_null(),
                        }
                    }
                    DataType::Binary => {
                        binary = true;
                        let b = any
                            .downcast_mut::<BinaryBuilder>()
                            .ok_or_else(AppError::internal)?;
                        match value {
                            ValueRef::Blob(v) | ValueRef::Text(v) => b.append_value(v),
                            _ => b.append_null(),
                        }
                    }
                    _ => {
                        let b = any
                            .downcast_mut::<StringBuilder>()
                            .ok_or_else(AppError::internal)?;
                        match value {
                            ValueRef::Text(v) => b.append_value(
                                std::str::from_utf8(v).map_err(|_| AppError::internal())?,
                            ),
                            _ => b.append_null(),
                        }
                    }
                }
            }
            pending += 1;
            if pending == 8192 {
                flush(&mut builders, &mut writer)?;
                pending = 0;
                if self.file.metadata().map_err(AppError::io)?.len() > self.policy.scratch_bytes {
                    return Err(AppError::limit("Export staging exceeded the disk budget"));
                }
            }
            Ok(())
        })?;
        if pending > 0 || written == 0 {
            flush(&mut builders, &mut writer)?;
        }
        writer.close().map_err(|e| {
            AppError::new(ErrorCode::Export, format!("Could not finish Parquet: {e}"))
        })?;
        Ok((written, binary))
    }
}
