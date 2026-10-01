use serde_json::Value;

use lumvise_plugin_protocol::PluginWireError;

/// Structured application failure returned to the invoking host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Optional machine-readable diagnostic context.
    pub details: Option<Value>,
    /// Whether retrying the operation may succeed.
    pub retryable: bool,
}

impl PluginError {
    /// Creates an application error without diagnostic details.
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            retryable,
        }
    }

    /// Creates the conventional error for an unexported capability.
    pub fn unknown_capability(capability_id: &str) -> Self {
        Self::new(
            "unknown_capability",
            format!(
                "unknown capability `{capability_id}`; expected a capability_id exported by this plugin"
            ),
            false,
        )
    }

    /// Attaches structured diagnostic context.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub(crate) fn degraded_protocol(message: impl Into<String>) -> Self {
        Self::new("degraded_protocol", message, false)
    }

    pub(crate) fn canceled(invocation_id: &str) -> Self {
        Self::new(
            "invocation_canceled",
            format!("invocation `{invocation_id}` was canceled by the host"),
            false,
        )
    }
}

impl From<PluginError> for PluginWireError {
    fn from(error: PluginError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            details: error.details,
            retryable: error.retryable,
        }
    }
}

impl From<PluginWireError> for PluginError {
    fn from(error: PluginWireError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            details: error.details,
            retryable: error.retryable,
        }
    }
}

/// Session-level failure that prevents the SDK from continuing safely.
#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    /// Framing, JSON, stream, or wire validation failed.
    #[error("plugin protocol failed: {0}")]
    Protocol(#[from] lumvise_plugin_protocol::ProtocolError),
    /// A complete frame could not be made visible to the host process.
    #[error("plugin output flush failed: {source}; expected the complete frame on stdout")]
    OutputFlush {
        /// Underlying output stream failure.
        #[source]
        source: std::io::Error,
    },
    /// Message belongs to a different live session.
    #[error("message session_id is `{actual}`; expected active session `{expected}`")]
    InvalidSession {
        /// Offending session identifier.
        actual: String,
        /// Active session identifier.
        expected: String,
    },
    /// Message is valid protocol JSON but invalid in the current SDK state.
    #[error("received message type `{actual}`; expected {expected}")]
    UnexpectedMessage {
        /// Offending message type.
        actual: &'static str,
        /// Expected message shape or state.
        expected: &'static str,
    },
    /// Session-owned synchronization state became unavailable.
    #[error("plugin session synchronization failed: {resource}")]
    SessionState {
        /// The affected session resource.
        resource: &'static str,
    },
}
