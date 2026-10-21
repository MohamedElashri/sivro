use super::{FilterExpr, FilterValue, Query};
use crate::{
    dataset::{ColumnDescriptor, ColumnKind, FilterOp},
    errors::{AppError, AppResult, ErrorCode},
    policy::ResourcePolicy,
};
use ::duckdb::{types::Value, Connection};
use std::collections::HashSet;

pub const VIEW: &str = "\"sivro_data\"";
pub const ORDINAL: &str = "\"__sivro_row\"";
pub const RAW_RECORD: &str = "\"__sivro_raw\"";
/// JSON record fields stored as exact source text in VARCHAR columns. Neither
/// is a DuckDB type name; they never reach a SQL cast.
pub const JSON_VALUE: &str = "JSON_VALUE";
pub const JSON_NUMBER: &str = "JSON_NUMBER";

pub fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
pub fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub fn classify_kind(data_type: &str) -> ColumnKind {
    let t = data_type.to_ascii_uppercase();
    if t == JSON_NUMBER {
        return ColumnKind::Number;
    }
    if t == "JSON"
        || t == JSON_VALUE
        || t.ends_with(']')
        || ["STRUCT(", "MAP(", "UNION(", "LIST(", "ARRAY("]
            .iter()
            .any(|k| t.starts_with(k))
    {
        ColumnKind::Nested
    } else if t == "BLOB" || t == "BINARY" {
        ColumnKind::Binary
    } else if t.starts_with("DECIMAL")
        || matches!(
            t.as_str(),
            "TINYINT"
                | "SMALLINT"
                | "INTEGER"
                | "BIGINT"
                | "HUGEINT"
                | "UTINYINT"
                | "USMALLINT"
                | "UINTEGER"
                | "UBIGINT"
                | "UHUGEINT"
        )
        || ["FLOAT", "DOUBLE", "BIGNUM"]
            .iter()
            .any(|k| t.starts_with(k))
    {
        ColumnKind::Number
    } else if t.starts_with("BOOL") {
        ColumnKind::Bool
    } else if ["DATE", "TIME", "INTERVAL"]
        .iter()
        .any(|k| t.starts_with(k))
    {
        ColumnKind::Date
    } else if ["VARCHAR", "CHAR", "STRING", "TEXT", "UUID", "JSON", "ENUM"]
        .iter()
        .any(|k| t.starts_with(k))
    {
        ColumnKind::Text
    } else {
        ColumnKind::Other
    }
}

pub fn operators(kind: ColumnKind) -> Vec<FilterOp> {
    use FilterOp::*;
    let mut result = vec![IsNull, IsNotNull];
    if kind == ColumnKind::Binary {
        return result;
    }
    if kind != ColumnKind::Nested {
        result.extend([Eq, Ne]);
    }
    if !matches!(kind, ColumnKind::Bool | ColumnKind::Nested) {
        result.extend([Gt, Lt, Gte, Lte]);
    }
    if kind != ColumnKind::Bool {
        result.extend([
            Contains,
            NotContains,
            ContainsAny,
            NotContainsAny,
            StartsWith,
            EndsWith,
        ]);
    }
    result
}

pub fn column<'a>(columns: &'a [ColumnDescriptor], id: &str) -> AppResult<&'a ColumnDescriptor> {
    if id.len() > 128 {
        return Err(AppError::invalid("Column ID is too long"));
    }
    columns.iter().find(|c| c.id == id).ok_or_else(|| {
        AppError::new(
            ErrorCode::InvalidColumn,
            "That column is not part of this object's schema",
        )
    })
}

pub fn projection(
    columns: &[ColumnDescriptor],
    ids: &[String],
    policy: &ResourcePolicy,
) -> AppResult<Vec<ColumnDescriptor>> {
    if ids.is_empty() || ids.len() > policy.max_columns {
        return Err(AppError::limit(
            "Select at least one column within the column limit",
        ));
    }
    let mut seen = HashSet::new();
    ids.iter()
        .map(|id| {
            if !seen.insert(id) {
                return Err(AppError::invalid("Column IDs must be unique"));
            }
            column(columns, id).cloned()
        })
        .collect()
}

pub fn display(column: &ColumnDescriptor) -> String {
    let name = quote_ident(&column.sql_name);
    if column.data_type == JSON_VALUE {
        // Already exact JSON text; to_json would quote it as a string.
        return name;
    }
    match column.kind {
        ColumnKind::Nested => {
            format!("CASE WHEN {name} IS NULL THEN NULL ELSE to_json({name})::VARCHAR END")
        }
        ColumnKind::Binary => format!("hex({name})"),
        _ => format!("{name}::VARCHAR"),
    }
}

pub struct CompiledQuery {
    pub selected: Vec<ColumnDescriptor>,
    pub predicate: String,
    pub order: String,
    pub parameters: Vec<Value>,
}

/// Canonical decimal spelling for comparison validation. No binary float is
/// involved when validating integer/DECIMAL filter values or their round trip.
fn exact_decimal(value: &str) -> Option<(bool, String, i64)> {
    let value = value.trim();
    let negative = value.starts_with('-');
    let value = value.strip_prefix(['-', '+']).unwrap_or(value);
    let mut e = value.split(['e', 'E']);
    let mantissa = e.next()?;
    let exponent: i64 = e.next().map(str::parse).transpose().ok()?.unwrap_or(0);
    if e.next().is_some() || exponent.unsigned_abs() > 1_000_000 {
        return None;
    }
    let mut parts = mantissa.split('.');
    let whole = parts.next()?;
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut digits = format!("{whole}{fraction}")
        .trim_start_matches('0')
        .to_string();
    if digits.is_empty() {
        return Some((false, "0".into(), 0));
    }
    let trailing = digits.len() - digits.trim_end_matches('0').len();
    digits.truncate(digits.len() - trailing);
    Some((
        negative,
        digits,
        exponent - fraction.len() as i64 + trailing as i64,
    ))
}

/// RFC 8259 number grammar: optional minus, no leading zeros, optional
/// fraction and exponent.
pub fn is_json_number(value: &str) -> bool {
    let b = value.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            digits(&mut i);
        }
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

/// serde_json stores an exponent as `e` plus an explicit sign (`1E5` becomes
/// `1e+5`); digits and signs are otherwise kept exactly.
pub fn json_number_spelling(token: &str) -> String {
    match token.split_once(['e', 'E']) {
        Some((mantissa, exponent)) if exponent.starts_with(['+', '-']) => {
            format!("{mantissa}e{exponent}")
        }
        Some((mantissa, exponent)) => format!("{mantissa}e+{exponent}"),
        None => token.to_string(),
    }
}

fn comparison(
    conn: &Connection,
    column: &ColumnDescriptor,
    value: &FilterValue,
    params: &mut Vec<Value>,
) -> AppResult<String> {
    let wrong = || {
        AppError::invalid(format!(
            "Expected a valid {} value for {}",
            column.data_type, column.name
        ))
    };
    if column.data_type == JSON_NUMBER {
        // No exact SQL numeric type holds this token: compare its exact text.
        let FilterValue::Number { value } = value else {
            return Err(wrong());
        };
        let v = value.trim();
        if !is_json_number(v) {
            return Err(AppError::invalid(format!(
                "Enter a JSON number token for {}; it is compared as exact source text",
                column.name
            )));
        }
        params.push(Value::Text(json_number_spelling(v)));
        return Ok("?".into());
    }
    let text = match (column.kind, value) {
        (ColumnKind::Number, FilterValue::Number { value }) => {
            let v = value.trim();
            if column.data_type.contains("INT") || column.data_type == "BIGNUM" {
                let digits = v.strip_prefix(['-', '+']).unwrap_or(v);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(wrong());
                }
            } else if exact_decimal(v).is_none() {
                return Err(wrong());
            }
            v.to_string()
        }
        (ColumnKind::Bool, FilterValue::Boolean { value }) => value.to_string(),
        (ColumnKind::Date, FilterValue::Temporal { value }) => {
            let v = value.trim();
            let ty = column.data_type.as_str();
            if ty == "DATE"
                && !v.eq_ignore_ascii_case("infinity")
                && !v.eq_ignore_ascii_case("-infinity")
                && (!v.bytes().all(|c| c.is_ascii_digit() || c == b'-')
                    || v.matches('-').count() != 2)
            {
                return Err(AppError::invalid(
                    "Use an ISO date without a time component",
                ));
            }
            let precision = match ty {
                "TIMESTAMP_NS" => 9,
                "TIMESTAMP_MS" => 3,
                "TIMESTAMP_S" => 0,
                _ => 6,
            };
            if let Some((_, fraction)) = v.split_once('.') {
                if fraction
                    .bytes()
                    .take_while(u8::is_ascii_digit)
                    .skip(precision)
                    .any(|c| c != b'0')
                {
                    return Err(AppError::invalid(
                        "The filter timestamp/time has more precision than the source type",
                    ));
                }
            }
            v.to_string()
        }
        (ColumnKind::Text | ColumnKind::Other, FilterValue::Text { value }) => value.clone(),
        _ => return Err(wrong()),
    };
    let ty = &column.data_type; // backend-discovered DuckDB type, never a client string
    let round_trip: Option<String> = conn
        .query_row(
            &format!("SELECT TRY_CAST(? AS {ty})::VARCHAR"),
            [&text],
            |r| r.get(0),
        )
        .map_err(|_| wrong())?;
    let cast = round_trip.ok_or_else(wrong)?;
    if ty.starts_with("DECIMAL") && exact_decimal(&text) != exact_decimal(&cast) {
        return Err(AppError::invalid(format!(
            "Value does not fit {ty} without rounding"
        )));
    }
    if matches!(ty.as_str(), "FLOAT" | "DOUBLE") && !cast.parse::<f64>().is_ok_and(f64::is_finite) {
        return Err(wrong());
    }
    params.push(Value::Text(text));
    Ok(format!("CAST(? AS {ty})"))
}

fn like_escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn predicate(
    conn: &Connection,
    expr: &FilterExpr,
    columns: &[ColumnDescriptor],
    params: &mut Vec<Value>,
) -> AppResult<String> {
    match expr {
        FilterExpr::All { terms } | FilterExpr::Any { terms } => {
            if terms.is_empty() {
                return Ok(if matches!(expr, FilterExpr::All { .. }) {
                    "true"
                } else {
                    "false"
                }
                .into());
            }
            let join = if matches!(expr, FilterExpr::All { .. }) {
                " AND "
            } else {
                " OR "
            };
            let parts: AppResult<Vec<_>> = terms
                .iter()
                .map(|e| predicate(conn, e, columns, params))
                .collect();
            Ok(format!("({})", parts?.join(join)))
        }
        FilterExpr::Predicate {
            column_id,
            op,
            value,
        } => {
            use FilterOp::*;
            let c = column(columns, column_id)?;
            if !c.operators.contains(op) {
                return Err(AppError::invalid(
                    "This operator is unavailable for the column's type",
                ));
            }
            let name = quote_ident(&c.sql_name);
            if matches!(op, IsNull | IsNotNull) {
                if !matches!(value, FilterValue::None) {
                    return Err(AppError::invalid("NULL filters do not accept a value"));
                }
                return Ok(format!(
                    "{name} IS {}NULL",
                    if *op == IsNotNull { "NOT " } else { "" }
                ));
            }
            if matches!(op, Eq | Ne | Gt | Lt | Gte | Lte) {
                let right = comparison(conn, c, value, params)?;
                let operator = match op {
                    Eq => "=",
                    Ne => "!=",
                    Gt => ">",
                    Lt => "<",
                    Gte => ">=",
                    _ => "<=",
                };
                return Ok(format!("{name} {operator} {right}"));
            }
            let FilterValue::Text { value } = value else {
                return Err(AppError::invalid("Text matching requires a text value"));
            };
            let terms: Vec<_> = if matches!(op, ContainsAny | NotContainsAny) {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect()
            } else {
                vec![value.as_str()]
            };
            if terms.is_empty() || terms.len() > 64 {
                return Err(AppError::limit("Use between 1 and 64 text matching terms"));
            }
            let not = matches!(op, NotContains | NotContainsAny);
            let parts: Vec<_> = terms
                .into_iter()
                .map(|v| {
                    let v = like_escape(v);
                    params.push(Value::Text(match op {
                        StartsWith => format!("{v}%"),
                        EndsWith => format!("%{v}"),
                        _ => format!("%{v}%"),
                    }));
                    format!(
                        "{} {}ILIKE ? ESCAPE '\\'",
                        display(c),
                        if not { "NOT " } else { "" }
                    )
                })
                .collect();
            let joined = parts.join(if not { " AND " } else { " OR " });
            Ok(if not {
                format!("({name} IS NULL OR ({joined}))")
            } else {
                format!("({joined})")
            })
        }
    }
}

pub fn compile(
    conn: &Connection,
    columns: &[ColumnDescriptor],
    query: &Query,
    policy: &ResourcePolicy,
) -> AppResult<CompiledQuery> {
    query.validate(policy)?;
    let selected = projection(columns, &query.column_ids, policy)?;
    let mut parameters = Vec::new();
    let mut filter = predicate(conn, &query.filter, columns, &mut parameters)?;
    if !query.quick_search.trim().is_empty() {
        let mut searchable: Vec<_> = columns
            .iter()
            .filter(|c| c.kind == ColumnKind::Text)
            .collect();
        if searchable.is_empty() {
            searchable = columns
                .iter()
                .filter(|c| c.kind != ColumnKind::Binary)
                .collect();
        }
        let parts: Vec<_> = searchable
            .into_iter()
            .map(|c| {
                parameters.push(Value::Text(format!(
                    "%{}%",
                    like_escape(query.quick_search.trim())
                )));
                format!("{} ILIKE ? ESCAPE '\\'", display(c))
            })
            .collect();
        if !parts.is_empty() {
            filter = format!("({filter}) AND ({})", parts.join(" OR "));
        }
    }
    let order = if let Some(sort) = &query.sort {
        let c = column(columns, &sort.column_id)?;
        if !c.sortable {
            return Err(AppError::invalid(format!(
                "{} holds raw JSON tokens/values; sorting them would compare text, not values",
                c.name
            )));
        }
        format!(
            "ORDER BY {}, {ORDINAL}",
            format_args!(
                "{} {}",
                quote_ident(&c.sql_name),
                if sort.descending { "DESC" } else { "ASC" }
            )
        )
    } else {
        // Preserve the adapter's insertion-order stream. Ordering explicitly
        // by the generated ordinal would force a global Top-N/scan just to
        // show the first unsorted page.
        String::new()
    };
    Ok(CompiledQuery {
        selected,
        predicate: format!("WHERE {filter}"),
        order,
        parameters,
    })
}
