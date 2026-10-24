//! Shared streaming columnar writers for native Arrow and SQLite exports.
use crate::{
    api::ExportFormat,
    errors::{AppError, AppResult, ErrorCode},
};
use arrow_array::RecordBatch;
use arrow_ipc::writer::{FileWriter, StreamWriter};
use arrow_schema::SchemaRef;
use std::fs::File;

pub enum Writer {
    Parquet(parquet::arrow::ArrowWriter<File>),
    Arrow(FileWriter<File>),
    Stream(StreamWriter<File>),
}
fn error(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::Export,
        format!("Could not write columnar output: {error}"),
    )
}
impl Writer {
    pub fn new(file: File, schema: SchemaRef, format: ExportFormat) -> AppResult<Self> {
        match format {
            ExportFormat::Parquet => parquet::arrow::ArrowWriter::try_new(file, schema, None)
                .map(Self::Parquet)
                .map_err(error),
            ExportFormat::Arrow => FileWriter::try_new(file, &schema)
                .map(Self::Arrow)
                .map_err(error),
            ExportFormat::ArrowStream => StreamWriter::try_new(file, &schema)
                .map(Self::Stream)
                .map_err(error),
            _ => Err(AppError::internal()),
        }
    }
    pub fn write(&mut self, batch: &RecordBatch) -> AppResult<()> {
        match self {
            Self::Parquet(w) => w.write(batch).map_err(error),
            Self::Arrow(w) => w.write(batch).map_err(error),
            Self::Stream(w) => w.write(batch).map_err(error),
        }
    }
    pub fn finish(self) -> AppResult<()> {
        match self {
            Self::Parquet(w) => w.close().map(|_| ()).map_err(error),
            Self::Arrow(mut w) => w.finish().map_err(error),
            Self::Stream(mut w) => w.finish().map_err(error),
        }
    }
}
