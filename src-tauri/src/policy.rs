use crate::errors::{AppError, AppResult};
use serde::{Deserialize, Serialize};

/// Product budgets, not a promise of hard process RSS isolation. Clients may
/// reduce these limits; increasing a default requires a deliberate code change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourcePolicy {
    pub default_page_size: usize,
    pub max_page_size: usize,
    pub schema_sample_rows: usize,
    pub json_document_bytes: u64,
    pub json_depth: usize,
    pub json_children: usize,
    pub jsonl_record_bytes: u64,
    pub cell_preview_bytes: usize,
    pub full_cell_bytes: usize,
    pub blob_preview_bytes: usize,
    pub hdf5_file_bytes: u64,
    pub hdf5_read_bytes: u64,
    pub hdf5_cells: usize,
    pub excel_workbook_bytes: u64,
    pub xls_workbook_bytes: u64,
    pub excel_payload_bytes: u64,
    pub excel_entry_bytes: u64,
    pub excel_cells: usize,
    pub scratch_bytes: u64,
    pub duckdb_memory_bytes: u64,
    pub expensive_operations: usize,
    pub timeout_ms: u64,
    pub default_export_rows: u64,
    pub max_export_rows: u64,
    pub max_sessions: usize,
    pub max_pending_operations: usize,
    pub max_columns: usize,
    pub max_filter_nodes: usize,
    pub max_filter_depth: usize,
    pub max_value_bytes: usize,
    pub max_search_bytes: usize,
    pub max_page_bytes: usize,
    pub max_cache_entries: usize,
    pub disk_reserve_bytes: u64,
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            default_page_size: 100,
            max_page_size: 1000,
            schema_sample_rows: 20_480,
            json_document_bytes: 32 << 20,
            json_depth: 64,
            json_children: 100,
            jsonl_record_bytes: 16 << 20,
            cell_preview_bytes: 4096,
            full_cell_bytes: 1 << 20,
            blob_preview_bytes: 128,
            hdf5_file_bytes: 4 << 30,
            hdf5_read_bytes: 64 << 20,
            hdf5_cells: 2_000_000,
            excel_workbook_bytes: 50 << 20,
            xls_workbook_bytes: 16 << 20,
            excel_payload_bytes: 256 << 20,
            excel_entry_bytes: 128 << 20,
            excel_cells: 200_000,
            scratch_bytes: 1 << 30,
            duckdb_memory_bytes: 512 << 20,
            expensive_operations: 2,
            timeout_ms: 30_000,
            default_export_rows: 100_000,
            max_export_rows: 10_000_000,
            max_sessions: 32,
            max_pending_operations: 128,
            max_columns: 4096,
            max_filter_nodes: 64,
            max_filter_depth: 8,
            max_value_bytes: 1 << 20,
            max_search_bytes: 4096,
            max_page_bytes: 8 << 20,
            max_cache_entries: 32,
            disk_reserve_bytes: 32 << 20,
        }
    }
}

impl ResourcePolicy {
    pub fn validate(&self) -> AppResult<()> {
        let defaults = Self::default();
        let value = serde_json::to_value(self).map_err(|_| AppError::internal())?;
        let upper = serde_json::to_value(defaults).map_err(|_| AppError::internal())?;
        for (key, v) in value.as_object().ok_or_else(AppError::internal)? {
            let n = v.as_u64().ok_or_else(AppError::internal)?;
            if n == 0 || n > upper[key].as_u64().ok_or_else(AppError::internal)? {
                return Err(AppError::limit(format!(
                    "Resource limit {key} must be positive and cannot exceed the product default"
                )));
            }
        }
        if self.default_page_size > self.max_page_size
            || self.cell_preview_bytes > self.full_cell_bytes
            || self.default_export_rows > self.max_export_rows
            || self.excel_entry_bytes > self.excel_payload_bytes
        {
            return Err(AppError::invalid("Resource limits are inconsistent"));
        }
        Ok(())
    }
}
