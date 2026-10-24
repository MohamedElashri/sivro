pub mod arrow;
pub mod delimited;
pub mod duckdb;
pub mod excel;
pub mod hdf5;
pub mod json;
pub mod parquet;
pub mod sqlite;
pub mod stage;

use crate::{
    api::*,
    dataset::FormatKind,
    errors::{AppError, AppResult, ErrorCode},
    policy::ResourcePolicy,
    query::Query,
    registry::Fingerprint,
    scratch::SessionScratch,
    sessions::OperationContext,
};
use std::{path::Path, sync::Arc};

/// Table operations shared by every tabular reader (DuckDB-backed formats
/// and SQLite), so sessions and commands do not depend on the engine.
pub trait TableSource {
    fn fetch_page(
        &self,
        generation: u64,
        query: &Query,
        policy: &ResourcePolicy,
    ) -> AppResult<PageResult>;
    fn count(
        &mut self,
        generation: u64,
        query: &Query,
        policy: &ResourcePolicy,
    ) -> AppResult<CountResult>;
    fn profile(
        &mut self,
        generation: u64,
        query: &ProfileQuery,
        policy: &ResourcePolicy,
    ) -> AppResult<ColumnProfile>;
    fn read_row(
        &self,
        generation: u64,
        query: &RowQuery,
        policy: &ResourcePolicy,
    ) -> AppResult<RowResult>;
    fn export(
        &mut self,
        generation: u64,
        options: ExportOptions,
        policy: &ResourcePolicy,
        context: &OperationContext,
    ) -> AppResult<ExportResult>;
    /// Tests drive long native work directly on the DuckDB connection.
    #[cfg(test)]
    fn duckdb(&mut self) -> Option<&mut duckdb::DuckDbAdapter> {
        None
    }
}

impl TableSource for duckdb::DuckDbAdapter {
    fn fetch_page(&self, g: u64, q: &Query, p: &ResourcePolicy) -> AppResult<PageResult> {
        duckdb::DuckDbAdapter::fetch_page(self, g, q, p)
    }
    fn count(&mut self, g: u64, q: &Query, p: &ResourcePolicy) -> AppResult<CountResult> {
        duckdb::DuckDbAdapter::count(self, g, q, p)
    }
    fn profile(
        &mut self,
        g: u64,
        q: &ProfileQuery,
        p: &ResourcePolicy,
    ) -> AppResult<ColumnProfile> {
        duckdb::DuckDbAdapter::profile(self, g, q, p)
    }
    fn read_row(&self, g: u64, q: &RowQuery, p: &ResourcePolicy) -> AppResult<RowResult> {
        duckdb::DuckDbAdapter::read_row(self, g, q, p)
    }
    fn export(
        &mut self,
        g: u64,
        options: ExportOptions,
        p: &ResourcePolicy,
        context: &OperationContext,
    ) -> AppResult<ExportResult> {
        if options.format == ExportFormat::Hdf5 {
            let source = hdf5::ExportSource {
                object: self.object.clone(),
                path: self.path.clone(),
                fingerprint: self.fingerprint.clone(),
            };
            return hdf5::export(self, g, options, source, p, context);
        }
        crate::export::export(self, g, options, p, context)
    }
    #[cfg(test)]
    fn duckdb(&mut self) -> Option<&mut duckdb::DuckDbAdapter> {
        Some(self)
    }
}

/// Cancellation reaches whichever native engine runs the operation.
pub enum Interrupt {
    DuckDb(Arc<::duckdb::InterruptHandle>),
    Sqlite(rusqlite::InterruptHandle),
}
impl Interrupt {
    pub fn interrupt(&self) {
        match self {
            Self::DuckDb(handle) => handle.interrupt(),
            Self::Sqlite(handle) => handle.interrupt(),
        }
    }
}

/// A session's reader: a DuckDB-backed table, or a bounded JSON document.
pub enum Adapter {
    Table(Box<duckdb::DuckDbAdapter>),
    Document(Box<json::JsonDocument>),
    Sqlite(Box<sqlite::SqliteAdapter>),
}

impl Adapter {
    pub fn fingerprint(&self) -> &Arc<Fingerprint> {
        match self {
            Self::Table(a) => &a.fingerprint,
            Self::Document(d) => &d.fingerprint,
            Self::Sqlite(s) => &s.fingerprint,
        }
    }
    pub fn path(&self) -> &Path {
        match self {
            Self::Table(a) => &a.path,
            Self::Document(d) => &d.path,
            Self::Sqlite(s) => &s.path,
        }
    }
    pub fn descriptor(
        &self,
        session_id: &str,
        generation: u64,
        policy: &ResourcePolicy,
    ) -> DatasetDescriptor {
        match self {
            Self::Table(a) => a.descriptor(session_id, generation, policy),
            Self::Document(d) => d.descriptor(session_id, generation, policy),
            Self::Sqlite(s) => s.descriptor(session_id, generation, policy),
        }
    }
    /// Document reads are in-memory and bounded; they cannot be interrupted.
    pub fn interrupt(&self) -> Option<Interrupt> {
        match self {
            Self::Table(a) => Some(Interrupt::DuckDb(a.conn.interrupt_handle())),
            Self::Document(_) => None,
            Self::Sqlite(s) => Some(Interrupt::Sqlite(s.conn.get_interrupt_handle())),
        }
    }
    /// Detects a changed source at operation boundaries. SQLite data is live;
    /// its adapter only treats replacement and schema changes as stale.
    pub fn check_source(&mut self) -> AppResult<()> {
        match self {
            Self::Sqlite(s) => s.check_source(),
            _ => self.fingerprint().check(self.path()),
        }
    }
    pub fn table(&mut self) -> AppResult<&mut dyn TableSource> {
        match self {
            Self::Table(a) => Ok(a.as_mut()),
            Self::Sqlite(s) => Ok(s.as_mut()),
            Self::Document(_) => Err(AppError::new(
                ErrorCode::InvalidOperation,
                "This JSON document has no table view; inspect or copy its nodes instead",
            )),
        }
    }
    pub fn document(&self) -> AppResult<&json::JsonDocument> {
        match self {
            Self::Document(d) => Ok(d),
            Self::Table(_) | Self::Sqlite(_) => Err(AppError::new(
                ErrorCode::InvalidOperation,
                "This source is a table; document reads apply to JSON documents only",
            )),
        }
    }
}

/// Opens or reopens one source with its frozen settings and effective format.
pub fn open(
    path: &Path,
    format: FormatKind,
    delimited: Option<delimited::FrozenDelimited>,
    options: &OpenOptions,
    object_id: String,
    scratch: SessionScratch,
    policy: &ResourcePolicy,
) -> AppResult<Adapter> {
    match (format, delimited) {
        (FormatKind::Csv | FormatKind::Tsv, Some(frozen)) => {
            delimited::DelimitedAdapter::open(path, object_id, frozen, scratch, policy)
                .map(|a| Adapter::Table(Box::new(a)))
        }
        (FormatKind::Json | FormatKind::Jsonl, None) => {
            json::open(path, format, object_id, scratch, policy)
        }
        (FormatKind::Excel, None) => {
            let options = options.excel.as_ref().ok_or_else(|| {
                AppError::invalid("Choose a worksheet and header row before opening a workbook")
            })?;
            excel::open(path, options, object_id, scratch, policy)
                .map(|a| Adapter::Table(Box::new(a)))
        }
        (FormatKind::Sqlite, None) => {
            let options = options.sqlite.as_ref().ok_or_else(|| {
                AppError::invalid("Choose a table or view before opening a database")
            })?;
            sqlite::open(path, options, object_id, policy).map(|a| Adapter::Sqlite(Box::new(a)))
        }
        (FormatKind::Hdf5, None) => {
            let selection = options.hdf5.as_ref().ok_or_else(|| {
                AppError::invalid("Choose an HDF5 dataset or column group before opening")
            })?;
            hdf5::open(path, selection, object_id, scratch, policy)
                .map(|a| Adapter::Table(Box::new(a)))
        }
        (FormatKind::Arrow, None) => {
            arrow::open(path, object_id, scratch, policy).map(|a| Adapter::Table(Box::new(a)))
        }
        (FormatKind::Parquet, None) => {
            parquet::ParquetAdapter::open(path, object_id, scratch, policy)
                .map(|a| Adapter::Table(Box::new(a)))
        }
        _ => Err(AppError::new(
            ErrorCode::UnsupportedFormat,
            "This format family is planned; its reader is not implemented yet",
        )),
    }
}
