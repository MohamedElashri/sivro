pub mod duckdb;
pub mod sqlite;

use crate::{
    dataset::FilterOp,
    errors::{AppError, AppResult},
    policy::ResourcePolicy,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FilterValue {
    None,
    Text { value: String },
    Number { value: String },
    Boolean { value: bool },
    Temporal { value: String },
    Json { value: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FilterExpr {
    All {
        terms: Vec<FilterExpr>,
    },
    Any {
        terms: Vec<FilterExpr>,
    },
    Predicate {
        column_id: String,
        op: FilterOp,
        value: FilterValue,
    },
}

impl Default for FilterExpr {
    fn default() -> Self {
        Self::All { terms: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SortSpec {
    pub column_id: String,
    pub descending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Query {
    pub column_ids: Vec<String>,
    pub offset: u64,
    pub page_size: usize,
    #[serde(default)]
    pub filter: FilterExpr,
    #[serde(default)]
    pub quick_search: String,
    pub sort: Option<SortSpec>,
}

impl Query {
    pub fn context_key(&self) -> AppResult<String> {
        serde_json::to_string(&(&self.filter, &self.quick_search)).map_err(|_| AppError::internal())
    }
    pub fn unfiltered(&self) -> bool {
        matches!(&self.filter, FilterExpr::All { terms } if terms.is_empty())
            && self.quick_search.trim().is_empty()
    }
    pub fn validate(&self, policy: &ResourcePolicy) -> AppResult<()> {
        if self.column_ids.is_empty() || self.column_ids.len() > policy.max_columns {
            return Err(AppError::limit(
                "Select a bounded, nonempty list of columns",
            ));
        }
        if self.page_size == 0 || self.page_size > policy.max_page_size {
            return Err(AppError::limit(format!(
                "Page size must be between 1 and {}",
                policy.max_page_size
            )));
        }
        if self.offset > 9_007_199_254_740_991 || self.quick_search.len() > policy.max_search_bytes
        {
            return Err(AppError::limit(
                "Page offset or search text exceeds the supported limit",
            ));
        }
        fn walk(
            expr: &FilterExpr,
            depth: usize,
            remaining: &mut usize,
            p: &ResourcePolicy,
        ) -> AppResult<()> {
            if depth > p.max_filter_depth || *remaining == 0 {
                return Err(AppError::limit(
                    "Filter expression is too large or deeply nested",
                ));
            }
            *remaining -= 1;
            match expr {
                FilterExpr::All { terms } | FilterExpr::Any { terms } => {
                    if terms.len() > p.max_filter_nodes {
                        return Err(AppError::limit("Too many filters"));
                    }
                    for term in terms {
                        walk(term, depth + 1, remaining, p)?;
                    }
                }
                FilterExpr::Predicate {
                    column_id, value, ..
                } => {
                    if column_id.len() > 128 {
                        return Err(AppError::invalid("Invalid column ID"));
                    }
                    let bytes = match value {
                        FilterValue::Text { value }
                        | FilterValue::Number { value }
                        | FilterValue::Temporal { value }
                        | FilterValue::Json { value } => value.len(),
                        _ => 0,
                    };
                    if bytes > p.max_value_bytes {
                        return Err(AppError::limit("Filter value is too large"));
                    }
                }
            }
            Ok(())
        }
        let mut remaining = policy.max_filter_nodes;
        walk(&self.filter, 0, &mut remaining, policy)
    }
}
