//! Validation failures retain the offending value across adapters.
use thiserror::Error;

pub type SemanticRevisionResult<T> = Result<T, SemanticRevisionError>;

#[derive(Debug, Error)]
pub enum SemanticRevisionError {
    #[error("invalid value {value}; expected {expected}")]
    InvalidValue { value: String, expected: String },
    #[error("revision payload encoding failed: {0}")]
    Encoding(#[from] serde_json::Error),
}

impl SemanticRevisionError {
    /// Names the rejected value and rule. Example: `SemanticRevisionError::invalid_value("", "non-empty ID")`.
    pub fn invalid_value(value: impl Into<String>, expected: impl Into<String>) -> Self {
        Self::InvalidValue {
            value: value.into(),
            expected: expected.into(),
        }
    }
}
