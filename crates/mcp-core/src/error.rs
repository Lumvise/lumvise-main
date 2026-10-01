use thiserror::Error;

pub type Result<T> = std::result::Result<T, McpCoreError>;

#[derive(Debug, Error)]
pub enum McpCoreError {
    #[error("json operation failed: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io operation failed: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid request value {value}; expected {expected}")]
    InvalidRequest { value: String, expected: String },
}

impl McpCoreError {
    pub fn invalid_request(value: impl Into<String>, expected: impl Into<String>) -> Self {
        Self::InvalidRequest {
            value: value.into(),
            expected: expected.into(),
        }
    }
}
