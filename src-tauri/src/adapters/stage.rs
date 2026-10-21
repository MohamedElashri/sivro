//! Session-scratch staging shared by readers that parse in Rust (JSON/JSONL,
//! Excel). Values are appended as exact text; only a fixed view applies the
//! exact SQL casts chosen after reading every value.
use crate::{
    dataset::{ColumnDescriptor, FilterOp},
    errors::{AppError, AppResult},
    policy::ResourcePolicy,
    query::duckdb as sql,
    scratch::SessionScratch,
};
use duckdb::{types::Value, Appender, Connection};

const FILE: &str = "records.duckdb";

/// Attaches `records.duckdb` in session scratch with an ordinal, an optional
/// source-record column and `fields` text columns.
pub fn create<'a>(
    conn: &'a Connection,
    scratch: &SessionScratch,
    fields: usize,
) -> AppResult<Appender<'a>> {
    let path = scratch.path.join(FILE);
    conn.execute_batch(&format!(
        "ATTACH {} AS sivro_stage; CREATE TABLE sivro_stage.records (ordinal BIGINT, raw VARCHAR{});",
        sql::quote_literal(
            path.to_str()
                .ok_or_else(|| AppError::invalid("Invalid scratch path"))?
        ),
        (0..fields)
            .map(|i| format!(", field_{i} VARCHAR"))
            .collect::<String>()
    ))
    .map_err(AppError::query)?;
    conn.appender_to_catalog_and_db("records", "sivro_stage", "main")
        .map_err(AppError::query)
}

pub fn append(
    appender: &mut Appender<'_>,
    ordinal: i64,
    raw: Option<String>,
    values: Vec<Option<String>>,
) -> AppResult<()> {
    let mut row = Vec::with_capacity(values.len() + 2);
    row.push(Value::BigInt(ordinal));
    row.push(raw.map_or(Value::Null, Value::Text));
    row.extend(
        values
            .into_iter()
            .map(|v| v.map_or(Value::Null, Value::Text)),
    );
    appender
        .append_row(duckdb::appender_params_from_iter(row))
        .map_err(AppError::query)
}

/// Refuses staging that has outgrown the session scratch budget.
pub fn check_size(scratch: &SessionScratch, policy: &ResourcePolicy) -> AppResult<()> {
    let bytes: u64 = [FILE, "records.duckdb.wal"]
        .iter()
        .filter_map(|name| std::fs::metadata(scratch.path.join(name)).ok())
        .map(|m| m.len())
        .sum();
    if bytes > policy.scratch_bytes {
        return Err(AppError::limit("Staged values exceeded the scratch budget"));
    }
    Ok(())
}

/// The query view: exact casts for typed columns, raw text otherwise.
pub fn create_view(conn: &Connection, types: &[String], raw: bool) -> AppResult<()> {
    let projection = types
        .iter()
        .enumerate()
        .map(|(i, ty)| {
            let name = format!("field_{i}");
            if ty == "VARCHAR" || ty == sql::JSON_VALUE || ty == sql::JSON_NUMBER {
                name
            } else {
                format!("CAST({name} AS {ty}) AS {name}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let raw = if raw {
        format!(", raw AS {}", sql::RAW_RECORD)
    } else {
        String::new()
    };
    conn.execute_batch(&format!(
        "CREATE TEMP VIEW {} AS SELECT {projection}, ordinal AS {}{raw} FROM sivro_stage.main.records",
        sql::VIEW,
        sql::ORDINAL,
    ))
    .map_err(AppError::query)
}

/// Raw JSON text and tokens cannot be ordered or range-compared by value.
pub fn column(index: usize, name: String, data_type: String, nullable: bool) -> ColumnDescriptor {
    let kind = sql::classify_kind(&data_type);
    let mut operators = sql::operators(kind);
    if data_type == sql::JSON_NUMBER {
        use FilterOp::*;
        operators.retain(|op| !matches!(op, Gt | Lt | Gte | Lte));
    }
    ColumnDescriptor {
        id: format!("col-{index}"),
        name,
        sql_name: format!("field_{index}"),
        sortable: data_type != sql::JSON_VALUE && data_type != sql::JSON_NUMBER,
        data_type,
        kind,
        nullable,
        operators,
    }
}
