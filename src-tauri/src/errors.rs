use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidInput,
    UnsupportedFormat,
    FormatMismatch,
    InvalidSession,
    InvalidObject,
    InvalidColumn,
    InvalidOperation,
    StaleDataset,
    InvalidGeneration,
    ResourceLimit,
    Cancelled,
    Timeout,
    Busy,
    Io,
    Query,
    Export,
    Internal,
}

#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }
    pub fn limit(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ResourceLimit, message)
    }
    pub fn query(error: impl std::fmt::Display) -> Self {
        Self::new(
            ErrorCode::Query,
            format!("Could not query this view: {error}"),
        )
    }
    pub fn io(error: impl std::fmt::Display) -> Self {
        Self::new(ErrorCode::Io, format!("Could not access the file: {error}"))
    }
    pub fn internal() -> Self {
        Self::new(
            ErrorCode::Internal,
            "The operation could not finish safely. Reopen the file and try again.",
        )
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for AppError {}
