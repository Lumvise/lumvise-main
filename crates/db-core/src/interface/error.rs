use thiserror::Error;

/// DB Core result type.
pub type Result<T> = std::result::Result<T, DbError>;

/// Phase in which a PZ snapshot operation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PzFailurePhase {
    Capture,
    Encode,
    Validate,
    Publish,
    Cancellation,
}

impl std::fmt::Display for PzFailurePhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Capture => "capture",
            Self::Encode => "encode",
            Self::Validate => "validate",
            Self::Publish => "publish",
            Self::Cancellation => "cancellation",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error("database operation failed: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("graph database operation failed: {0}")]
    Grafeo(String),
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("json operation failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("vectorizer operation failed: {0}")]
    Vectorizer(String),
    #[error("invalid value {value}; expected {expected}")]
    InvalidValue { value: String, expected: String },
    #[error("PZ snapshot {phase} phase failed: {message}")]
    Pz {
        phase: PzFailurePhase,
        message: String,
    },
    #[error("unresolved semantic commit {commit_version} with state {state}")]
    UnresolvedCommit { commit_version: i64, state: String },
}

impl DbError {
    /// Creates a validation error that names the offending value.
    ///
    /// # Example
    ///
    /// ```
    /// use lumvise_db_core::DbError;
    /// let error = DbError::invalid_value("", "non-empty id");
    /// assert!(error.to_string().contains("non-empty id"));
    /// ```
    pub fn invalid_value(value: impl Into<String>, expected: impl Into<String>) -> Self {
        Self::InvalidValue {
            value: value.into(),
            expected: expected.into(),
        }
    }

    /// Creates a phase-preserving PZ snapshot error.
    pub fn pz(phase: PzFailurePhase, message: impl Into<String>) -> Self {
        Self::Pz {
            phase,
            message: message.into(),
        }
    }

    /// Creates a vectorizer integration error.
    ///
    /// # Example
    ///
    /// ```
    /// let error = lumvise_db_core::DbError::vectorizer("engine failed");
    /// assert!(error.to_string().contains("engine failed"));
    /// ```
    pub fn vectorizer(message: impl Into<String>) -> Self {
        Self::Vectorizer(message.into())
    }

    /// Creates an unresolved semantic commit error.
    ///
    /// # Example
    ///
    /// ```
    /// let error = lumvise_db_core::DbError::unresolved_commit(2, "failed");
    /// assert!(error.to_string().contains("unresolved semantic commit"));
    /// ```
    pub fn unresolved_commit(commit_version: i64, state: impl Into<String>) -> Self {
        Self::UnresolvedCommit {
            commit_version,
            state: state.into(),
        }
    }
}
