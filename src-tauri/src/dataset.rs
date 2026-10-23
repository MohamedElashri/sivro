use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatKind {
    Parquet,
    Hdf5,
    Csv,
    Tsv,
    Json,
    Jsonl,
    Excel,
    Sqlite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColumnKind {
    Number,
    Text,
    Bool,
    Date,
    Nested,
    Binary,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Lt,
    Gte,
    Lte,
    Contains,
    NotContains,
    ContainsAny,
    NotContainsAny,
    StartsWith,
    EndsWith,
    IsNull,
    IsNotNull,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ColumnDescriptor {
    pub id: String,
    pub name: String,
    pub sql_name: String,
    pub data_type: String,
    pub kind: ColumnKind,
    pub nullable: bool,
    pub operators: Vec<FilterOp>,
    /// False when ordering would compare raw JSON text instead of values.
    #[serde(default = "sortable_default")]
    pub sortable: bool,
}
fn sortable_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CountState {
    Known { value: String },
    Unknown,
    Pending,
    Failed { reason: String },
}
impl CountState {
    pub fn known(count: u64) -> Self {
        Self::Known {
            value: count.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Cell {
    Null,
    Boolean {
        value: bool,
    },
    Number {
        value: String,
        logical_type: String,
        byte_length: String,
        truncated: bool,
    },
    Text {
        value: String,
        byte_length: String,
        truncated: bool,
    },
    Json {
        value: String,
        byte_length: String,
        truncated: bool,
    },
    Binary {
        byte_count: String,
        preview_hex: String,
        truncated: bool,
    },
}

impl Cell {
    pub fn payload_bytes(&self) -> usize {
        match self {
            Self::Null | Self::Boolean { .. } => 8,
            Self::Number { value, .. } | Self::Text { value, .. } | Self::Json { value, .. } => {
                value.len()
            }
            Self::Binary { preview_hex, .. } => preview_hex.len(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub page: bool,
    pub filter: bool,
    pub sort: bool,
    pub profile: bool,
    pub exact_count: bool,
    pub copy: bool,
    pub exports: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    Table,
    Document,
    Scalar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectDescriptor {
    pub id: String,
    pub label: String,
    pub kind: ObjectKind,
    pub columns: Vec<ColumnDescriptor>,
    pub row_count: CountState,
    pub capabilities: Capabilities,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowGroupInfo {
    pub index: usize,
    pub num_rows: String,
    pub bytes: String,
    pub num_columns: usize,
}

/// Generated IDs make exported/copy object keys unambiguous while preserving
/// duplicate/blank original labels in the grid.
pub fn ambiguous_labels(columns: &[ColumnDescriptor]) -> bool {
    let mut names = std::collections::HashSet::new();
    columns
        .iter()
        .any(|c| c.name.is_empty() || !names.insert(c.name.to_lowercase()))
}
