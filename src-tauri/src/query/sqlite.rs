//! SQLite SELECT compiler. Identifiers come only from introspection and are
//! quoted for SQLite; every filter value is a bound parameter. DuckDB SQL is
//! not reused: affinity, LIKE and type ordering differ.
use super::{duckdb::column, duckdb::projection, FilterExpr, FilterValue, Query};
use crate::{
    dataset::{ColumnDescriptor, ColumnKind, FilterOp},
    errors::{AppError, AppResult},
    policy::ResourcePolicy,
};
use rusqlite::types::Value;

/// The per-session CTE every query reads from; its columns are `c0..cn`.
pub const SOURCE: &str = "sivro_src";

pub fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// SQLite column affinity rules (datatype3.html, section 3.1), in order.
pub fn affinity(declared: &str) -> &'static str {
    let t = declared.to_ascii_uppercase();
    if t.contains("INT") {
        "INTEGER"
    } else if ["CHAR", "CLOB", "TEXT"].iter().any(|k| t.contains(k)) {
        "TEXT"
    } else if t.is_empty() || t.contains("BLOB") {
        "BLOB"
    } else if ["REAL", "FLOA", "DOUB"].iter().any(|k| t.contains(k)) {
        "REAL"
    } else {
        "NUMERIC"
    }
}

/// Kinds only steer filter controls: any SQLite cell may hold any storage
/// class, and cells always carry their own runtime type.
pub fn kind(declared: &str) -> ColumnKind {
    let upper = declared.to_ascii_uppercase();
    match affinity(declared) {
        "INTEGER" | "REAL" => ColumnKind::Number,
        "NUMERIC" if upper.contains("DATE") || upper.contains("TIME") => ColumnKind::Date,
        "NUMERIC" => ColumnKind::Number,
        "TEXT" => ColumnKind::Text,
        _ => ColumnKind::Other,
    }
}

pub fn operators(kind: ColumnKind) -> Vec<FilterOp> {
    use FilterOp::*;
    let mut ops = vec![IsNull, IsNotNull];
    if kind != ColumnKind::Other {
        ops.extend([Eq, Ne, Gt, Lt, Gte, Lte]);
    }
    ops.extend([
        Contains,
        NotContains,
        ContainsAny,
        NotContainsAny,
        StartsWith,
        EndsWith,
    ]);
    ops
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

/// Text matching on the value's text form. SQLite LIKE folds ASCII case only;
/// BLOB values never match instead of being decoded.
fn matches_text(name: &str) -> String {
    format!(
        "coalesce(CASE WHEN typeof({name}) = 'blob' THEN 0 ELSE CAST({name} AS TEXT) LIKE ? ESCAPE '\\' END, 0)"
    )
}

/// Integers bind exactly as INTEGER; other numbers bind as REAL, the same
/// binary double SQLite uses for a numeric literal.
fn number(value: &str, name: &str) -> AppResult<Value> {
    let v = value.trim();
    let digits = v.strip_prefix(['-', '+']).unwrap_or(v);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        return v.parse::<i64>().map(Value::Integer).map_err(|_| {
            AppError::invalid(format!("{name}: integers must fit SQLite's 64-bit range"))
        });
    }
    match v.parse::<f64>() {
        Ok(f) if f.is_finite() && !v.contains(['i', 'I', 'n', 'N']) => Ok(Value::Real(f)),
        _ => Err(AppError::invalid(format!(
            "Enter a finite number for {name}"
        ))),
    }
}

fn comparison(c: &ColumnDescriptor, value: &FilterValue) -> AppResult<Value> {
    match (c.kind, value) {
        (ColumnKind::Number, FilterValue::Number { value }) => number(value, &c.name),
        (ColumnKind::Text, FilterValue::Text { value })
        | (ColumnKind::Date, FilterValue::Temporal { value }) => Ok(Value::Text(value.clone())),
        _ => Err(AppError::invalid(format!(
            "This comparison is unavailable for {}",
            c.name
        ))),
    }
}

fn predicate(
    expr: &FilterExpr,
    columns: &[ColumnDescriptor],
    params: &mut Vec<Value>,
) -> AppResult<String> {
    match expr {
        FilterExpr::All { terms } | FilterExpr::Any { terms } => {
            let all = matches!(expr, FilterExpr::All { .. });
            if terms.is_empty() {
                return Ok(if all { "1" } else { "0" }.into());
            }
            let parts = terms
                .iter()
                .map(|t| predicate(t, columns, params))
                .collect::<AppResult<Vec<_>>>()?;
            Ok(format!(
                "({})",
                parts.join(if all { " AND " } else { " OR " })
            ))
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
                params.push(comparison(c, value)?);
                let operator = match op {
                    Eq => "=",
                    Ne => "!=",
                    Gt => ">",
                    Lt => "<",
                    Gte => ">=",
                    _ => "<=",
                };
                return Ok(format!("{name} {operator} ?"));
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
                    matches_text(&name)
                })
                .collect();
            Ok(if not {
                format!("({name} IS NULL OR NOT ({}))", parts.join(" OR "))
            } else {
                format!("({})", parts.join(" OR "))
            })
        }
    }
}

pub struct Compiled {
    pub selected: Vec<ColumnDescriptor>,
    pub predicate: String,
    pub order: String,
    pub params: Vec<Value>,
}

/// `key` is the adapter's stable-order expression (rowid, primary key or
/// position); it breaks ties so paging is deterministic for tables.
pub fn compile(
    columns: &[ColumnDescriptor],
    query: &Query,
    key: &str,
    policy: &ResourcePolicy,
) -> AppResult<Compiled> {
    query.validate(policy)?;
    let selected = projection(columns, &query.column_ids, policy)?;
    let mut params = Vec::new();
    let mut filter = predicate(&query.filter, columns, &mut params)?;
    let search = query.quick_search.trim();
    if !search.is_empty() {
        let mut searchable: Vec<_> = columns
            .iter()
            .filter(|c| c.kind == ColumnKind::Text)
            .collect();
        if searchable.is_empty() {
            searchable = columns.iter().collect();
        }
        let parts: Vec<_> = searchable
            .iter()
            .map(|c| {
                params.push(Value::Text(format!("%{}%", like_escape(search))));
                matches_text(&quote_ident(&c.sql_name))
            })
            .collect();
        filter = format!("({filter}) AND ({})", parts.join(" OR "));
    }
    let order = match &query.sort {
        Some(sort) => {
            let c = column(columns, &sort.column_id)?;
            format!(
                "ORDER BY {} {}, {key}",
                quote_ident(&c.sql_name),
                if sort.descending { "DESC" } else { "ASC" }
            )
        }
        None => format!("ORDER BY {key}"),
    };
    Ok(Compiled {
        selected,
        predicate: format!("WHERE {filter}"),
        order,
        params,
    })
}
