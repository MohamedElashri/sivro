use crate::dataset::{ColumnDescriptor, ColumnKind, FormatKind};
use crate::{
    adapters::duckdb::DuckDbAdapter,
    api::*,
    errors::{AppError, AppResult, ErrorCode},
    policy::ResourcePolicy,
    query::duckdb as sql,
    scratch::free_space,
    sessions::OperationContext,
};
use duckdb::params_from_iter;
use serde::Deserialize;
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

static NEXT_STAGE: AtomicU64 = AtomicU64::new(1);

pub struct ExportStage {
    pub file: PathBuf,
    pub destination: PathBuf,
    directory: PathBuf,
}
impl ExportStage {
    pub fn new(
        destination: &str,
        format: ExportFormat,
        source: Option<&Path>,
        policy: &ResourcePolicy,
    ) -> AppResult<Self> {
        if destination.is_empty() || destination.len() > 32_768 {
            return Err(AppError::invalid("Choose a bounded export path"));
        }
        let raw = Path::new(destination);
        let extension = raw
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let valid = match format {
            ExportFormat::Hdf5 => matches!(extension.as_str(), "h5" | "hdf5" | "hdf"),
            ExportFormat::Parquet => matches!(extension.as_str(), "parquet" | "pq"),
            ExportFormat::Csv => extension == "csv",
            ExportFormat::Tsv => extension == "tsv",
            ExportFormat::Json => extension == "json",
            ExportFormat::Jsonl => matches!(extension.as_str(), "jsonl" | "ndjson"),
        };
        if !valid {
            return Err(AppError::invalid(
                "The destination extension must match the selected output format",
            ));
        }
        let parent = raw
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(AppError::io)?;
        let parent = parent.canonicalize().map_err(AppError::io)?;
        let destination = parent.join(
            raw.file_name()
                .ok_or_else(|| AppError::invalid("Choose an export filename"))?,
        );
        if destination.symlink_metadata().is_ok() {
            if let Some(source) = source {
                if same_file::is_same_file(source, &destination).map_err(AppError::io)? {
                    return Err(AppError::new(
                        ErrorCode::Export,
                        "The source file or an alias cannot be overwritten",
                    ));
                }
            }
            return Err(AppError::new(ErrorCode::Export, "The destination already exists. Choose a new filename; existing files are never replaced."));
        }
        free_space(&parent, policy, 0)?;
        let name = format!(
            ".sivro-export-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| AppError::internal())?
                .as_nanos(),
            NEXT_STAGE.fetch_add(1, Ordering::Relaxed)
        );
        let directory = parent.join(name);
        fs::create_dir(&directory).map_err(AppError::io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
                .map_err(AppError::io)?;
        }
        Ok(Self {
            file: directory.join(format!("data.{extension}")),
            destination,
            directory,
        })
    }
    pub fn publish(&self) -> AppResult<()> {
        // Windows can only flush a handle opened with write access.
        fs::OpenOptions::new()
            .write(true)
            .open(&self.file)
            .and_then(|file| file.sync_all())
            .map_err(AppError::io)?;
        // Atomic, no-clobber publication on the destination filesystem. An
        // existing symlink/hardlink/racing file cannot be overwritten by this.
        fs::hard_link(&self.file, &self.destination).map_err(|e| {
            AppError::new(
                ErrorCode::Export,
                format!("Could not publish the completed export without replacing a file: {e}"),
            )
        })
    }
}
impl Drop for ExportStage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub fn export(
    adapter: &mut DuckDbAdapter,
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
    let compiled = sql::compile(
        &adapter.conn,
        &adapter.object.columns,
        &options.view,
        policy,
    )?;
    if options.format == ExportFormat::Parquet {
        if let Some(c) = compiled
            .selected
            .iter()
            .find(|c| c.data_type == sql::JSON_NUMBER)
        {
            return Err(AppError::new(ErrorCode::Export, format!("{} holds JSON numbers with exponents or more than 38 digits; no Parquet numeric type stores them exactly. Hide the column or export JSON, JSONL, CSV or TSV.", c.name)));
        }
    }
    let stage = ExportStage::new(
        &options.destination,
        options.format,
        Some(&adapter.path),
        policy,
    )?;
    let fields = compiled
        .selected
        .iter()
        .map(|c| sql::quote_ident(&c.sql_name))
        .collect::<Vec<_>>()
        .join(", ");
    let select = format!(
        "SELECT {fields} FROM {} {} {} LIMIT {}",
        sql::VIEW,
        compiled.predicate,
        compiled.order,
        options.max_rows
    );
    let count_sql = format!(
        "SELECT count(*) FROM (SELECT {fields} FROM {} {} LIMIT {})",
        sql::VIEW,
        compiled.predicate,
        options.max_rows + 1
    );
    let found: u64 = adapter
        .conn
        .query_row(
            &count_sql,
            params_from_iter(compiled.parameters.iter()),
            |r| r.get(0),
        )
        .map_err(AppError::query)?;
    context.check()?;
    if matches!(options.format, ExportFormat::Csv | ExportFormat::Tsv) {
        let collision = compiled
            .selected
            .iter()
            .filter(|c| c.kind != ColumnKind::Binary)
            .map(|c| {
                format!(
                    "{}::VARCHAR = {}",
                    sql::quote_ident(&c.sql_name),
                    sql::quote_literal(r"\N")
                )
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let collision = if collision.is_empty() {
            "false".to_string()
        } else {
            collision
        };
        let exists: bool = adapter
            .conn
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM ({select}) WHERE {collision})"),
                params_from_iter(compiled.parameters.iter()),
                |r| r.get(0),
            )
            .map_err(AppError::query)?;
        if exists {
            return Err(AppError::new(ErrorCode::Export, "A value equals the CSV/TSV NULL marker \\N. Choose Parquet or JSON to avoid ambiguity."));
        }
    }
    let ambiguous = crate::dataset::ambiguous_labels(&adapter.object.columns);
    // JSON record and Excel tables hold exact tokens/tagged JSON as text;
    // DuckDB's JSON writer would quote them or reparse them as doubles.
    let token_writer = matches!(
        adapter.format,
        FormatKind::Json | FormatKind::Jsonl | FormatKind::Excel | FormatKind::Hdf5
    ) && matches!(options.format, ExportFormat::Json | ExportFormat::Jsonl);
    let labels = compiled
        .selected
        .iter()
        .map(|c| {
            let name = sql::quote_ident(&c.sql_name);
            let value = match (c.kind, options.format) {
                (ColumnKind::Binary, ExportFormat::Json | ExportFormat::Jsonl) => format!("CASE WHEN {name} IS NULL THEN NULL ELSE struct_pack(\"$binary_hex\" := hex({name}), \"byteCount\" := octet_length({name})::VARCHAR) END"),
                (ColumnKind::Binary, ExportFormat::Csv | ExportFormat::Tsv) => format!("CASE WHEN {name} IS NULL THEN NULL ELSE 'hex:' || hex({name}) END"),
                // DuckDB's Parquet writer stores 128-bit integers as DOUBLE.
                // DECIMAL(38,0) is exact; wider values fail the cast loudly.
                (ColumnKind::Number, ExportFormat::Parquet)
                    if matches!(c.data_type.as_str(), "HUGEINT" | "UHUGEINT") =>
                {
                    format!("CAST({name} AS DECIMAL(38,0))")
                }
                _ => name,
            };
            format!(
                "{} AS {}",
                value,
                sql::quote_ident(if ambiguous { &c.id } else { &c.name })
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let output = match options.format {
        ExportFormat::Hdf5 => return Err(AppError::internal()),
        ExportFormat::Parquet => "FORMAT PARQUET",
        ExportFormat::Csv => "FORMAT CSV, HEADER true, DELIMITER ',', NULL '\\N'",
        ExportFormat::Json => "FORMAT JSON, ARRAY true",
        ExportFormat::Jsonl => "FORMAT JSON, ARRAY false",
        ExportFormat::Tsv => "FORMAT CSV, HEADER true, DELIMITER '\t', NULL '\\N'",
    };
    let copy = format!(
        "COPY (SELECT {labels} FROM ({select})) TO {} ({output})",
        sql::quote_literal(
            stage
                .file
                .to_str()
                .ok_or_else(|| AppError::invalid("Invalid destination path"))?
        )
    );
    let stopped = AtomicBool::new(false);
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let written: u64 = if token_writer {
        let keys: Vec<&str> = compiled
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
        write_json_tokens(
            adapter,
            &compiled.selected,
            &keys,
            &select_tokens(&compiled, options.max_rows),
            &compiled.parameters,
            options.format == ExportFormat::Json,
            &stage.file,
            policy,
            context,
        )?
    } else {
        std::thread::scope(|scope| {
            let _stop = Stop(&stopped);
            scope.spawn(|| {
                while !stopped.load(Ordering::SeqCst) {
                    if let Ok(meta) = stage.file.metadata() {
                        if meta.len() > policy.scratch_bytes {
                            context
                                .cancel(AppError::limit("Export staging exceeded the disk budget"));
                            break;
                        }
                    }
                    if let Err(error) = free_space(&stage.directory, policy, 0) {
                        context.cancel(error);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            adapter
                .conn
                .query_row(&copy, params_from_iter(compiled.parameters.iter()), |r| {
                    r.get(0)
                })
                .map_err(AppError::query)
        })?
    };
    context.check()?;
    if stage.file.metadata().map_err(AppError::io)?.len() > policy.scratch_bytes {
        return Err(AppError::limit("Export staging exceeded the disk budget"));
    }
    if matches!(options.format, ExportFormat::Json | ExportFormat::Jsonl) {
        validate_json(&stage.file, options.format == ExportFormat::Json, context)?;
    }
    context.publish(|| {
        adapter.fingerprint.check(&adapter.path)?;
        stage.publish()
    })?;
    let mut warnings = Vec::new();
    if ambiguous {
        warnings.push("Duplicate/blank labels: column IDs are used as export keys/headers.".into());
    }
    if !token_writer
        && compiled
            .selected
            .iter()
            .any(|c| c.data_type == sql::JSON_VALUE)
    {
        warnings.push("Nested or mixed-type JSON fields are written as JSON text strings.".into());
    }
    if compiled
        .selected
        .iter()
        .any(|c| c.kind == ColumnKind::Binary)
    {
        match options.format {
            ExportFormat::Json | ExportFormat::Jsonl => {
                warnings.push("Binary cells use tagged hexadecimal objects in JSON.".into())
            }
            ExportFormat::Csv | ExportFormat::Tsv => {
                warnings.push("Binary cells use hex:-prefixed text in CSV/TSV.".into())
            }
            ExportFormat::Hdf5 | ExportFormat::Parquet => (),
        }
    }
    if found > options.max_rows {
        warnings.push(
            "The export reached the requested row cap; additional matching rows were omitted."
                .into(),
        );
    }
    Ok(ExportResult {
        session_generation: generation,
        object_id: adapter.object.id.clone(),
        destination: stage.destination.to_string_lossy().into_owned(),
        exported_rows: written.to_string(),
        truncated: found > options.max_rows,
        warnings,
    })
}

fn select_tokens(compiled: &sql::CompiledQuery, max_rows: u64) -> String {
    let fields = compiled
        .selected
        .iter()
        .map(sql::display)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "SELECT {fields} FROM {} {} {} LIMIT {max_rows}",
        sql::VIEW,
        compiled.predicate,
        compiled.order
    )
}

/// Streams JSON/JSONL records with exact numeric tokens and raw JSON values
/// inserted verbatim; only text cells are encoded as JSON strings.
#[allow(clippy::too_many_arguments)]
fn write_json_tokens(
    adapter: &DuckDbAdapter,
    columns: &[ColumnDescriptor],
    keys: &[&str],
    select: &str,
    parameters: &[duckdb::types::Value],
    array: bool,
    file: &Path,
    policy: &ResourcePolicy,
    context: &OperationContext,
) -> AppResult<u64> {
    let keys = keys
        .iter()
        .map(|k| serde_json::to_string(k).map_err(|_| AppError::internal()))
        .collect::<AppResult<Vec<_>>>()?;
    let mut out = std::io::BufWriter::new(
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file)
            .map_err(AppError::io)?,
    );
    let mut statement = adapter.conn.prepare(select).map_err(AppError::query)?;
    let mut rows = statement
        .query(params_from_iter(parameters.iter()))
        .map_err(AppError::query)?;
    let mut written = 0u64;
    let mut line = Vec::new();
    if array {
        out.write_all(b"[").map_err(AppError::io)?;
    }
    while let Some(row) = rows.next().map_err(AppError::query)? {
        line.clear();
        if array && written > 0 {
            line.push(b',');
        }
        if array {
            line.push(b'\n');
        }
        line.push(b'{');
        for (i, (column, key)) in columns.iter().zip(&keys).enumerate() {
            if i > 0 {
                line.push(b',');
            }
            line.extend_from_slice(key.as_bytes());
            line.push(b':');
            match row.get::<_, Option<String>>(i).map_err(AppError::query)? {
                None => line.extend_from_slice(b"null"),
                Some(value) if column.kind == ColumnKind::Binary => {
                    let tagged = serde_json::json!({ "$binary_hex": value, "byteCount": (value.len()/2).to_string() });
                    serde_json::to_writer(&mut line, &tagged).map_err(|_| AppError::internal())?;
                }
                Some(value)
                    if matches!(column.kind, ColumnKind::Number | ColumnKind::Bool)
                        || column.data_type == sql::JSON_VALUE =>
                {
                    line.extend_from_slice(value.as_bytes())
                }
                Some(value) => {
                    serde_json::to_writer(&mut line, &value).map_err(|_| AppError::internal())?
                }
            }
        }
        line.push(b'}');
        if !array {
            line.push(b'\n');
        }
        out.write_all(&line).map_err(AppError::io)?;
        written += 1;
        if written.is_multiple_of(1024) {
            context.check()?;
            out.flush().map_err(AppError::io)?;
            if file.metadata().map_err(AppError::io)?.len() > policy.scratch_bytes {
                return Err(AppError::limit("Export staging exceeded the disk budget"));
            }
            free_space(file.parent().unwrap_or(file), policy, 0)?;
        }
    }
    if array {
        out.write_all(if written > 0 { b"\n]\n" } else { b"]\n" })
            .map_err(AppError::io)?;
    }
    out.flush().map_err(AppError::io)?;
    Ok(written)
}

/// Streaming syntax validation catches non-standard JSON (such as NaN) in a
/// staged export without loading an export-sized value into memory.
pub fn validate_json(path: &Path, array: bool, context: &OperationContext) -> AppResult<()> {
    let file = File::open(path).map_err(AppError::io)?;
    struct CancellableRead<'a> {
        file: File,
        context: &'a OperationContext,
    }
    impl std::io::Read for CancellableRead<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.context.check().map_err(std::io::Error::other)?;
            std::io::Read::read(&mut self.file, buffer)
        }
    }
    let mut de = serde_json::Deserializer::from_reader(std::io::BufReader::new(CancellableRead {
        file,
        context,
    }));
    let validated = if array {
        serde::de::IgnoredAny::deserialize(&mut de).and_then(|_| de.end())
    } else {
        de.into_iter::<serde::de::IgnoredAny>()
            .try_for_each(|item| item.map(|_| ()))
    };
    validated.map_err(|_| AppError::new(ErrorCode::Export, "These values cannot be represented as standard JSON (for example NaN/Infinity). Choose Parquet to preserve native types."))
}
