use thiserror::Error;

pub type Result<T> = std::result::Result<T, NeuralError>;

#[derive(Debug, Error)]
pub enum NeuralError {
    #[error("invalid value `{value}`; expected {expected}")]
    InvalidValue { value: String, expected: String },
    #[error("missing value `{value}`; expected {expected}")]
    MissingValue { value: String, expected: String },
    #[error("process `{command}` failed with status {status}; {diagnostics}")]
    ProcessFailed {
        command: String,
        status: String,
        /// What the process reported before failing, bounded. Carries stdout as
        /// well as stderr: a CLI may report its only actionable reason on
        /// stdout (#88).
        diagnostics: crate::process::ProcessFailureDiagnostics,
    },
    #[error("process `{command}` timed out after {timeout_ms}ms; expected completion")]
    ProcessTimeout { command: String, timeout_ms: u64 },
    #[error("process `{command}` cancelled by caller before completion")]
    ProcessCancelled { command: String },
    #[error("malformed payload `{value}`; expected {expected}")]
    MalformedPayload { value: String, expected: String },
    #[error("provider `{provider_id}` failed: {message}")]
    ProviderFailed {
        provider_id: String,
        message: String,
    },
    #[error("provider `{provider_id}` exhausted its bounded MCP tool rounds")]
    ToolRoundsExhausted { provider_id: String },
    #[error("db core error: {0}")]
    DbCore(String),
    #[error("io error for `{value}`; expected {expected}: {source}")]
    Io {
        value: String,
        expected: String,
        #[source]
        source: std::io::Error,
    },
    #[error("json error for `{value}`; expected {expected}: {source}")]
    Json {
        value: String,
        expected: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("engine `{engine_id}` is still warming up after waiting {waited_ms}ms")]
    WarmingUp { engine_id: String, waited_ms: u64 },
}

impl From<lumvise_db_core::DbError> for NeuralError {
    fn from(error: lumvise_db_core::DbError) -> Self {
        Self::DbCore(error.to_string())
    }
}

pub(crate) fn require_non_empty(value: &str, expected: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(NeuralError::InvalidValue {
            value: value.to_string(),
            expected: expected.to_string(),
        });
    }
    Ok(())
}
