//! CSV/TSV share one strict DuckDB scanner. Detection is only a preview;
//! committed sessions use explicit column types and frozen parsing options.
use super::duckdb::{connection, literal_source_pattern, with_deadline, DuckDbAdapter};
use crate::{
    dataset::*,
    errors::{AppError, AppResult},
    policy::ResourcePolicy,
    query::duckdb as sql,
    registry::Fingerprint,
    scratch::SessionScratch,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelimitedOptions {
    /// None means the family's default; "auto" requests dialect detection.
    pub delimiter: Option<String>,
    pub header: Option<bool>,
    pub quote: Option<String>,
    pub escape: Option<String>,
    pub null_marker: Option<String>,
    #[serde(default)]
    pub force_text: bool,
    #[serde(default)]
    pub type_overrides: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenOptions {
    pub type_overrides: BTreeMap<String, String>,
    pub delimiter: String,
    pub header: bool,
    pub quote: String,
    pub escape: String,
    pub null_marker: String,
    pub force_text: bool,
}
#[derive(Debug, Clone)]
pub struct FrozenDelimited {
    pub format: FormatKind,
    pub options: FrozenOptions,
    pub columns: Vec<ColumnDescriptor>,
    pub warnings: Vec<String>,
}
pub struct PreparedDelimited {
    pub path: PathBuf,
    pub fingerprint: Arc<Fingerprint>,
    pub frozen: FrozenDelimited,
    pub created: Instant,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DelimitedPreview {
    pub token: String,
    pub format: FormatKind,
    pub options: FrozenOptions,
    pub columns: Vec<ColumnDescriptor>,
    pub rows: Vec<Vec<Cell>>,
    pub sampled_rows: usize,
    pub has_more: bool,
    pub warnings: Vec<String>,
}

fn validate_character(value: &str, empty: bool) -> AppResult<()> {
    if (empty && value.is_empty())
        || (value.len() == 1
            && value.as_bytes()[0].is_ascii()
            && !matches!(value.as_bytes()[0], 0 | b'\r' | b'\n'))
    {
        return Ok(());
    }
    Err(AppError::invalid(
        "Delimiter, quote and escape must each be one ASCII character (quote/escape may be empty)",
    ))
}
pub fn type_override(value: &str) -> AppResult<String> {
    let value = value.trim().to_ascii_uppercase();
    if matches!(
        value.as_str(),
        "VARCHAR" | "BIGINT" | "UBIGINT" | "HUGEINT" | "BOOLEAN" | "DATE" | "TIMESTAMP" | "DOUBLE"
    ) {
        return Ok(value);
    }
    if let Some(body) = value
        .strip_prefix("DECIMAL(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let parts: Vec<_> = body.split(',').map(str::trim).collect();
        if parts.len() == 2 {
            if let (Ok(p), Ok(s)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) {
                if (1..=38).contains(&p) && s <= p {
                    return Ok(format!("DECIMAL({p},{s})"));
                }
            }
        }
    }
    Err(AppError::invalid("Type override must be VARCHAR, BIGINT, UBIGINT, HUGEINT, BOOLEAN, DATE, TIMESTAMP, DOUBLE or DECIMAL(p,s) with precision at most 38"))
}
/// Conservative exact inference: no automatic binary floating-point or date
/// conversion. Integer width/decimal scale are inferred from original text.
fn infer(values: &[Option<String>]) -> String {
    let nonnull: Vec<_> = values.iter().flatten().collect();
    if nonnull.is_empty() {
        return "VARCHAR".into();
    }
    if nonnull
        .iter()
        .all(|v| v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("false"))
    {
        return "BOOLEAN".into();
    }
    if nonnull.iter().all(|v| v.parse::<i64>().is_ok()) {
        return "BIGINT".into();
    }
    if nonnull.iter().all(|v| v.parse::<i128>().is_ok()) {
        return "HUGEINT".into();
    }
    let mut whole = 0usize;
    let mut scale = 0usize;
    for value in nonnull {
        let value = value.strip_prefix(['-', '+']).unwrap_or(value);
        let mut parts = value.split('.');
        let integer = parts.next().unwrap_or("");
        let fraction = parts.next().unwrap_or("");
        if parts.next().is_some()
            || (integer.is_empty() && fraction.is_empty())
            || !integer
                .bytes()
                .chain(fraction.bytes())
                .all(|c| c.is_ascii_digit())
        {
            return "VARCHAR".into();
        }
        whole = whole.max(integer.trim_start_matches('0').len());
        scale = scale.max(fraction.len());
    }
    // DECIMAL(s,s) prints ".5"; keep one integer digit so values stay readable.
    let precision = whole.max(1) + scale;
    if precision > 38 || scale > 38 {
        "VARCHAR".into()
    } else {
        format!("DECIMAL({precision},{scale})")
    }
}
fn source_sql(
    path: &Path,
    frozen: &FrozenDelimited,
    raw: bool,
    null_override: Option<&str>,
) -> AppResult<String> {
    let o = &frozen.options;
    let columns = frozen
        .columns
        .iter()
        .map(|c| {
            format!(
                "{}: {}",
                sql::quote_literal(&c.sql_name),
                sql::quote_literal("VARCHAR")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!("read_csv({}, auto_detect=false, columns={{{columns}}}, header={}, delim={}, quote={}, escape={}, nullstr={}, allow_quoted_nulls=false, strict_mode=true, ignore_errors=false, null_padding=false, parallel=false, compression='none', encoding='utf-8', max_line_size=16777216, buffer_size=67108864)",
        sql::quote_literal(&literal_source_pattern(path)?), if raw { false } else { o.header }, sql::quote_literal(&o.delimiter), sql::quote_literal(&o.quote), sql::quote_literal(&o.escape), sql::quote_literal(null_override.unwrap_or(&o.null_marker))))
}
fn parse_error(error: impl std::fmt::Display) -> AppError {
    AppError::invalid(format!("Could not parse UTF-8 delimited text: {error}. Check delimiter/header/quote/NULL settings, or reopen with force-text for type mismatches. No malformed rows are intentionally skipped."))
}

pub struct DelimitedAdapter;
impl DelimitedAdapter {
    pub fn preview(
        path: &Path,
        format: FormatKind,
        options: DelimitedOptions,
        token: String,
        scratch: SessionScratch,
        policy: &ResourcePolicy,
    ) -> AppResult<(PreparedDelimited, DelimitedPreview)> {
        let conn = connection(&scratch, policy)?;
        with_deadline(&conn, policy, || {
            Self::preview_inner(path, format, options, token, policy, &conn)
        })
    }
    fn preview_inner(
        path: &Path,
        format: FormatKind,
        options: DelimitedOptions,
        token: String,
        policy: &ResourcePolicy,
        conn: &duckdb::Connection,
    ) -> AppResult<(PreparedDelimited, DelimitedPreview)> {
        let fingerprint = Arc::new(Fingerprint::read(path)?);
        let delimiter = options
            .delimiter
            .clone()
            .unwrap_or_else(|| if format == FormatKind::Tsv { "\t" } else { "," }.into());
        if delimiter != "auto" {
            validate_character(&delimiter, false)?;
        }
        for value in [&options.quote, &options.escape].into_iter().flatten() {
            validate_character(value, true)?;
        }
        let null_marker = options.null_marker.clone().unwrap_or_else(|| r"\N".into());
        if null_marker.len() > 128 || null_marker.contains(['\0', '\r', '\n']) {
            return Err(AppError::invalid(
                "NULL marker must be at most 128 bytes and contain no line breaks or NUL",
            ));
        }
        if options.type_overrides.len() > policy.max_columns {
            return Err(AppError::limit("Too many type overrides"));
        }
        let mut params = format!(
            "sample_size={}, skip=0, comment='', nullstr={}",
            policy.schema_sample_rows,
            sql::quote_literal(&null_marker)
        );
        if delimiter != "auto" {
            params.push_str(&format!(", delim={}", sql::quote_literal(&delimiter)));
        }
        if let Some(header) = options.header {
            params.push_str(&format!(", header={header}"));
        }
        let requested_quote = options.quote.as_deref().unwrap_or("\"");
        let requested_escape = options.escape.as_deref().unwrap_or(requested_quote);
        params.push_str(&format!(
            ", quote={}, escape={}",
            sql::quote_literal(requested_quote),
            sql::quote_literal(requested_escape)
        ));
        let sniff = format!(
            "SELECT Delimiter, Quote, Escape, HasHeader, len(Columns) FROM sniff_csv({}, {params})",
            sql::quote_literal(&literal_source_pattern(path)?)
        );
        let (delimiter, quote, escape, header, width): (String, String, String, bool, usize) = conn
            .query_row(&sniff, [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(parse_error)?;
        let decode = |value: String| match value.as_str() {
            "(empty)" => String::new(),
            "\\t" => "\t".into(),
            _ => value,
        };
        let (delimiter, quote, escape) = (decode(delimiter), decode(quote), decode(escape));
        validate_character(&delimiter, false)?;
        validate_character(&quote, true)?;
        validate_character(&escape, true)?;
        if width == 0 || width > policy.max_columns {
            return Err(AppError::limit(
                "Delimited column count exceeds the schema budget",
            ));
        }
        let columns = (0..width)
            .map(|i| ColumnDescriptor {
                id: format!("col-{i}"),
                name: format!("Column {}", i + 1),
                sql_name: format!("field_{i}"),
                data_type: "VARCHAR".into(),
                kind: ColumnKind::Text,
                nullable: true,
                operators: sql::operators(ColumnKind::Text),
                sortable: true,
            })
            .collect();
        let mut frozen = FrozenDelimited { format, options: FrozenOptions { delimiter, quote, escape, header, null_marker, force_text: options.force_text, type_overrides: options.type_overrides.clone() }, columns, warnings: vec!["UTF-8 (optional BOM) only. Schema is inferred from a bounded sample and frozen; later type mismatches fail. Reopen as text to preserve identifiers/lexical spelling. Blank physical lines are ignored by the CSV scanner.".into(), "Unquoted fields equal to the NULL marker are NULL; quoted matching text and empty text remain text unless an empty NULL marker is selected.".into()] };
        let source = source_sql(path, &frozen, true, None)?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT * FROM {source} LIMIT {}",
                policy.schema_sample_rows + usize::from(header) + 1
            ))
            .map_err(parse_error)?;
        let mut rows = stmt.query([]).map_err(parse_error)?;
        let mut sample = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = rows.next().map_err(parse_error)? {
            let values = (0..width)
                .map(|i| row.get::<_, Option<String>>(i).map_err(parse_error))
                .collect::<AppResult<Vec<_>>>()?;
            bytes += values.iter().flatten().map(|v| v.len() + 64).sum::<usize>();
            if bytes > policy.max_page_bytes {
                return Err(AppError::limit(
                    "Schema sample exceeds 8 MiB; reduce input width/value sizes",
                ));
            }
            sample.push(values);
        }
        if sample.is_empty() {
            return Err(AppError::invalid("The file has no delimited records"));
        }
        if header {
            sample.remove(0);
            let labels = read_labels(conn, path, &frozen)?;
            for (column, label) in frozen.columns.iter_mut().zip(labels) {
                column.name = label;
            }
        }
        let has_more = sample.len() > policy.schema_sample_rows;
        sample.truncate(policy.schema_sample_rows);
        for (i, column) in frozen.columns.iter_mut().enumerate() {
            let values = sample.iter().map(|r| r[i].clone()).collect::<Vec<_>>();
            column.data_type = if let Some(value) = options.type_overrides.get(&column.id) {
                type_override(value)?
            } else if options.force_text {
                "VARCHAR".into()
            } else {
                infer(&values)
            };
            column.kind = sql::classify_kind(&column.data_type);
            column.operators = sql::operators(column.kind);
        }
        for key in options.type_overrides.keys() {
            if !frozen.columns.iter().any(|c| &c.id == key) {
                return Err(AppError::invalid(
                    "A type override refers to an unknown column ID",
                ));
            }
        }
        // Validate exact decimal overrides against the raw sampled lexemes before
        // DuckDB can round them. Other casts are verified by a bounded read.
        validate_decimal_sample(&frozen, &sample)?;
        let typed = source_sql(path, &frozen, false, None)?;
        conn.execute_batch(&format!(
            "CREATE TEMP VIEW preview_data AS SELECT {} FROM {typed}",
            typed_projection(&frozen)
        ))
        .map_err(parse_error)?;
        let mut check = conn
            .prepare(&format!(
                "SELECT * FROM preview_data LIMIT {}",
                policy.schema_sample_rows
            ))
            .map_err(parse_error)?;
        let mut checked = check.query([]).map_err(parse_error)?;
        while checked.next().map_err(parse_error)?.is_some() {}
        let unique: std::collections::HashSet<_> = frozen
            .columns
            .iter()
            .map(|c| c.name.to_ascii_lowercase())
            .collect();
        if unique.len() != width || frozen.columns.iter().any(|c| c.name.is_empty()) {
            frozen.warnings.push("Duplicate/blank labels are retained. JSON row copy and exports use column IDs as keys/headers when labels are ambiguous.".into());
        }
        if frozen.columns.iter().any(|c| c.data_type == "DOUBLE") {
            frozen.warnings.push(
                "Explicit DOUBLE overrides use binary floating-point and may round decimal input."
                    .into(),
            );
        }
        let preview_rows = sample
            .iter()
            .take(20)
            .map(|row| {
                row.iter()
                    .map(|value| match value {
                        None => Cell::Null,
                        Some(value) => {
                            let mut end = value.len().min(policy.cell_preview_bytes);
                            while !value.is_char_boundary(end) {
                                end -= 1;
                            }
                            Cell::Text {
                                value: value[..end].into(),
                                byte_length: value.len().to_string(),
                                truncated: end < value.len(),
                            }
                        }
                    })
                    .collect()
            })
            .collect();
        fingerprint.check(path)?;
        let preview = DelimitedPreview {
            token,
            format,
            options: frozen.options.clone(),
            columns: frozen.columns.clone(),
            rows: preview_rows,
            sampled_rows: sample.len(),
            has_more,
            warnings: frozen.warnings.clone(),
        };
        if serde_json::to_vec(&preview)
            .map_err(|_| AppError::internal())?
            .len()
            > policy.max_page_bytes
        {
            return Err(AppError::limit(
                "Parsing preview exceeds the response budget",
            ));
        }
        Ok((
            PreparedDelimited {
                path: path.to_owned(),
                fingerprint,
                frozen,
                created: Instant::now(),
            },
            preview,
        ))
    }
    pub fn open(
        path: &Path,
        object_id: String,
        frozen: FrozenDelimited,
        scratch: SessionScratch,
        policy: &ResourcePolicy,
    ) -> AppResult<DuckDbAdapter> {
        let fingerprint = Arc::new(Fingerprint::read(path)?);
        let conn = connection(&scratch, policy)?;
        if frozen.options.header {
            let labels = with_deadline(&conn, policy, || read_labels(&conn, path, &frozen))?;
            if labels
                .iter()
                .zip(&frozen.columns)
                .any(|(label, column)| label != &column.name)
            {
                return Err(AppError::invalid(
                    "The header changed. Reopen Parsing settings and confirm a new schema.",
                ));
            }
        }
        let source = source_sql(path, &frozen, false, None)?;
        conn.execute_batch(&format!(
            "CREATE TEMP VIEW {} AS SELECT {}, row_number() OVER () - 1 AS {} FROM {source}",
            sql::VIEW,
            typed_projection(&frozen),
            sql::ORDINAL
        ))
        .map_err(parse_error)?;
        fingerprint.check(path)?;
        Ok(DuckDbAdapter {
            hdf5: None,
            format: frozen.format,
            delimited: Some(frozen.options.clone()),
            excel: None,
            conn,
            path: path.to_owned(),
            fingerprint,
            object: ObjectDescriptor {
                id: object_id,
                label: "Data".into(),
                kind: ObjectKind::Table,
                columns: frozen.columns,
                row_count: CountState::Unknown,
                capabilities: Capabilities {
                    page: true,
                    filter: true,
                    sort: true,
                    profile: true,
                    exact_count: true,
                    copy: true,
                    exports: vec![
                        "parquet".into(),
                        "hdf5".into(),
                        "csv".into(),
                        "tsv".into(),
                        "json".into(),
                        "jsonl".into(),
                    ],
                },
                warnings: frozen.warnings,
            },
            row_count: None,
            row_groups: vec![],
            created_by: None,
            format_version: 0,
            counts: HashMap::new(),
            profiles: HashMap::new(),
            raw_records: false,
            _scratch: scratch,
        })
    }
}
fn validate_decimal_sample(
    frozen: &FrozenDelimited,
    rows: &[Vec<Option<String>>],
) -> AppResult<()> {
    for (i, c) in frozen.columns.iter().enumerate() {
        if c.data_type.starts_with("DECIMAL(") {
            let scale: usize = c
                .data_type
                .split(',')
                .nth(1)
                .unwrap_or("0)")
                .trim_end_matches(')')
                .parse()
                .unwrap_or(0);
            for value in rows.iter().filter_map(|r| r[i].as_ref()) {
                if value
                    .split('.')
                    .nth(1)
                    .is_some_and(|s| s.trim_end_matches('0').len() > scale)
                {
                    return Err(AppError::invalid("A DECIMAL override would round sampled input; increase scale or choose VARCHAR"));
                }
            }
        }
    }
    Ok(())
}

fn typed_projection(frozen: &FrozenDelimited) -> String {
    frozen.columns.iter().map(|c| {
        let name = sql::quote_ident(&c.sql_name);
        let value = match c.data_type.as_str() {
            "VARCHAR" => name.clone(),
            "BIGINT" | "UBIGINT" | "HUGEINT" => format!("CASE WHEN {name} IS NULL THEN NULL WHEN regexp_full_match({name}, '[+-]?[0-9]+') THEN CAST({name} AS {}) ELSE error('Integer conversion would change input; reopen as VARCHAR') END", c.data_type),
            "DATE" => format!("CASE WHEN {name} IS NULL THEN NULL WHEN regexp_full_match({name}, '[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}') THEN CAST({name} AS DATE) ELSE error('DATE requires YYYY-MM-DD; reopen as VARCHAR') END"),
            "TIMESTAMP" => format!("CASE WHEN {name} IS NULL THEN NULL WHEN regexp_full_match({name}, '[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}[ T][0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}([.][0-9]{{1,6}})?') THEN CAST({name} AS TIMESTAMP) ELSE error('TIMESTAMP requires local seconds with at most six fractional digits; reopen as VARCHAR') END"),
            ty if ty.starts_with("DECIMAL(") => {
                let scale = ty.split(',').nth(1).unwrap_or("0)").trim_end_matches(')');
                format!("CASE WHEN {name} IS NULL THEN NULL WHEN regexp_full_match({name}, '[+-]?([0-9]+([.][0-9]*)?|[.][0-9]+)') AND length(rtrim(split_part({name}, '.', 2), '0')) <= {scale} THEN CAST({name} AS {ty}) ELSE error('Decimal conversion would round input; reopen with more scale or VARCHAR') END")
            },
            ty => format!("CAST({name} AS {ty})"),
        };
        format!("{value} AS {name}")
    }).collect::<Vec<_>>().join(", ")
}

fn read_labels(
    conn: &duckdb::Connection,
    path: &Path,
    frozen: &FrozenDelimited,
) -> AppResult<Vec<String>> {
    // Two different sentinels recover every literal header, including one
    // equal to the source NULL marker or either header sentinel itself.
    let read = |marker: &str| -> AppResult<Vec<Option<String>>> {
        let source = source_sql(path, frozen, true, Some(marker))?;
        conn.query_row(&format!("SELECT * FROM {source} LIMIT 1"), [], |row| {
            (0..frozen.columns.len()).map(|i| row.get(i)).collect()
        })
        .map_err(parse_error)
    };
    let first = read("__sivro_header_null_a__")?;
    if first.iter().all(Option::is_some) {
        return Ok(first.into_iter().flatten().collect());
    }
    let second = read("__sivro_header_null_b__")?;
    first
        .into_iter()
        .zip(second)
        .map(|(a, b)| a.or(b).ok_or_else(AppError::internal))
        .collect()
}
