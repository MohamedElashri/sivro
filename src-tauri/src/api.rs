use crate::{dataset::*, policy::ResourcePolicy, query::Query};
use serde::{Deserialize, Serialize};

pub const API_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenOptions {
    pub format: Option<FormatKind>,
    pub preview_token: Option<String>,
    /// Required for Excel: the worksheet and header row, frozen per session.
    pub excel: Option<ExcelOptions>,
    /// Required for SQLite: the table or view, frozen per session.
    pub sqlite: Option<SqliteOptions>,
    pub hdf5: Option<Hdf5Options>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hdf5Options {
    pub object: String,
    #[serde(default)]
    pub columns: bool,
    #[serde(default)]
    pub row_axis: usize,
    pub column_axis: Option<usize>,
    #[serde(default)]
    pub indices: Vec<String>,
    #[serde(default)]
    pub row_start: String,
    pub row_limit: Option<u64>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hdf5Attribute {
    pub name: String,
    pub data_type: String,
    pub value: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hdf5Object {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub shape: Vec<String>,
    pub data_type: String,
    pub layout: String,
    pub chunk_shape: Option<Vec<String>>,
    pub filters: Vec<u16>,
    pub attributes: Vec<Hdf5Attribute>,
    pub selectable: bool,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hdf5Info {
    pub group: String,
    pub attributes: Vec<Hdf5Attribute>,
    pub objects: Vec<Hdf5Object>,
    pub columns_selectable: bool,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hdf5Selection {
    pub options: Hdf5Options,
    pub info: Hdf5Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SqliteOptions {
    pub object: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DbObjectKind {
    Table,
    View,
    Virtual,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbObjectInfo {
    pub name: String,
    pub kind: DbObjectKind,
    pub columns: u32,
    pub without_rowid: bool,
    pub selectable: bool,
    /// Why an object cannot be opened, e.g. a virtual table's module.
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInfo {
    pub sqlite_version: String,
    pub journal_mode: String,
    pub objects: Vec<DbObjectInfo>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqliteSelection {
    pub object: String,
    pub database: DatabaseInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExcelOptions {
    pub sheet: String,
    /// 1-based sheet row holding labels; None means the sheet has no header.
    pub header_row: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetKind {
    Worksheet,
    Chartsheet,
    Dialogsheet,
    Macrosheet,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetVisibility {
    Visible,
    Hidden,
    VeryHidden,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetInfo {
    pub name: String,
    pub kind: SheetKind,
    pub visibility: SheetVisibility,
    /// Only worksheets hold a cell grid that Sivro reads.
    pub selectable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbookInfo {
    /// File extension family: xlsx, xlsm, xlsb or xls.
    pub variant: String,
    pub date1904: bool,
    pub sheets: Vec<SheetInfo>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcelSelection {
    pub sheet: String,
    pub header_row: Option<u32>,
    pub workbook: WorkbookInfo,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub format: FormatKind,
    pub supported: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatasetDescriptor {
    pub api_version: u32,
    pub session_id: String,
    pub generation: u64,
    pub format: FormatKind,
    pub delimited: Option<crate::adapters::delimited::FrozenOptions>,
    /// Display/recent-files only. Queries never accept it back as a source.
    pub path: String,
    pub file_name: String,
    pub file_size: String,
    pub object: ObjectDescriptor,
    pub row_groups: Vec<RowGroupInfo>,
    pub created_by: Option<String>,
    pub format_version: i32,
    pub policy: ResourcePolicy,
    pub document: Option<DocumentInfo>,
    pub excel: Option<ExcelSelection>,
    pub sqlite: Option<SqliteSelection>,
    pub hdf5: Option<Hdf5Selection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Object,
    Array,
    String,
    Number,
    Boolean,
    Null,
}

/// Why a JSON source uses the bounded document viewer instead of a table.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentInfo {
    pub root_kind: NodeKind,
    pub child_count: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentMode {
    Children,
    Copy,
}

/// Nodes are addressed by child positions, which stay unambiguous for
/// duplicate object keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentQuery {
    pub path: Vec<u64>,
    pub offset: u64,
    pub limit: usize,
    pub mode: DocumentMode,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentNode {
    pub index: u64,
    pub key: Option<String>,
    pub key_truncated: bool,
    pub kind: NodeKind,
    /// Decoded string text, or the exact source token for numbers.
    pub value: Option<String>,
    pub byte_length: String,
    pub truncated: bool,
    pub child_count: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentResult {
    pub session_generation: u64,
    pub object_id: String,
    pub path: Vec<u64>,
    pub node: DocumentNode,
    pub children: Vec<DocumentNode>,
    pub offset: u64,
    pub has_more: bool,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationRequest<T> {
    pub session_id: String,
    pub generation: u64,
    pub object_id: String,
    pub operation_id: String,
    pub query: T,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Page,
    Count,
    Profile,
    ReadRow,
    Export,
    Document,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationHandle {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelResult {
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseResult {
    pub closed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageResult {
    pub session_generation: u64,
    pub object_id: String,
    pub column_ids: Vec<String>,
    pub row_ids: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    pub offset: u64,
    pub limit: usize,
    pub has_more: bool,
    pub count: CountState,
    pub query_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CountResult {
    pub session_generation: u64,
    pub object_id: String,
    pub count: CountState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileQuery {
    pub column_id: String,
    pub context: Query,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopValue {
    pub value: Cell,
    pub count: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnProfile {
    pub session_generation: u64,
    pub object_id: String,
    pub column_id: String,
    pub scope: String,
    pub null_count: String,
    pub non_null_count: String,
    pub distinct_count: String,
    pub min: Cell,
    pub max: Cell,
    pub top_values: Vec<TopValue>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RowQuery {
    pub row_id: String,
    pub column_ids: Vec<String>,
    /// JSON/JSONL record tables retain each source record for inspection.
    #[serde(default)]
    pub raw_record: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowResult {
    pub session_generation: u64,
    pub object_id: String,
    pub row_id: String,
    pub column_ids: Vec<String>,
    pub cells: Vec<Cell>,
    pub raw_record: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Parquet,
    Hdf5,
    Csv,
    Tsv,
    Json,
    Jsonl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportOptions {
    pub destination: String,
    pub format: ExportFormat,
    pub max_rows: u64,
    pub view: Query,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub session_generation: u64,
    pub object_id: String,
    pub destination: String,
    pub exported_rows: String,
    pub truncated: bool,
    pub warnings: Vec<String>,
}
