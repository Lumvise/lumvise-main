use thiserror::Error;

/// Frontend Core result type.
pub type Result<T> = std::result::Result<T, FrontendError>;

#[derive(Debug, Error)]
pub enum FrontendError {
    #[error("invalid value `{value}`; expected {expected}")]
    InvalidValue { value: String, expected: String },
    #[error("missing value `{value}`; expected {expected}")]
    MissingValue { value: String, expected: String },
}

impl FrontendError {
    /// Creates a validation error that includes the offending value.
    ///
    /// # Example
    ///
    /// ```
    /// let error = lumvise_frontend_core::FrontendError::invalid_value("x", "known view");
    /// assert!(error.to_string().contains("known view"));
    /// ```
    pub fn invalid_value(value: impl Into<String>, expected: impl Into<String>) -> Self {
        Self::InvalidValue {
            value: value.into(),
            expected: expected.into(),
        }
    }
}
