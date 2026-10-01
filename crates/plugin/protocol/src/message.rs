use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ProtocolError;

/// Protocol version carried by every message.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtocolVersion {
    /// Breaking protocol generation.
    pub major: u16,
    /// Backwards-compatible feature generation.
    pub minor: u16,
}

impl ProtocolVersion {
    /// Creates a protocol version from its major and minor generations.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

/// Protocol implemented by this crate.
pub const CURRENT_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(6, 0);

/// Disposition of one dirty graph element in a [`StorageTriggerRequest`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageTriggerDisposition {
    /// The element is present and its current state should be reconciled.
    Upserted,
    /// The element is an inactive tombstone and should be removed by the consumer.
    Removal,
}

/// One element identity delivered to a compiled StorageTrigger.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageTriggerChanged {
    /// Stable graph entity identifier.
    pub entity_id: String,
    /// Stable graph entity kind.
    pub entity_kind: String,
    /// Whether the consumer should upsert or remove the current entity.
    pub disposition: StorageTriggerDisposition,
}

/// Current batch request delivered to a compiled StorageTrigger.
///
/// The request intentionally carries identities and revisions only. A trigger
/// reads current entity state through its declared read capability, so the
/// process-boundary payload does not duplicate graph content.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageTriggerRequest {
    /// Project whose graph changed.
    pub project_root: String,
    /// Revision immediately preceding this delivery.
    pub base_revision: i64,
    /// Latest revision represented by this delivery.
    pub target_revision: i64,
    /// Dirty entities, coalesced by entity identity.
    pub changed: Vec<StorageTriggerChanged>,
}

impl StorageTriggerRequest {
    /// Encodes this typed contract into the generic structured input accepted
    /// by [`MessageBody::HostInvoke`].
    pub fn to_value(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Decodes a typed StorageTrigger request from a generic invocation input.
    pub fn from_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }
}

/// Current response returned by a compiled StorageTrigger after reconciliation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorageTriggerResponse {
    /// True when the trigger reconciled the complete delivered batch.
    pub acknowledged: bool,
}

impl StorageTriggerResponse {
    /// Encodes this typed contract into a generic plugin result value.
    pub fn to_value(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Decodes a typed response from a generic plugin result value.
    pub fn from_value(value: serde_json::Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(value)
    }
}

/// One complete message sent across the plugin process boundary.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WireMessage {
    /// Version used to interpret the message.
    pub protocol: ProtocolVersion,
    /// Directional operation and its payload.
    #[serde(flatten)]
    pub body: MessageBody,
}

/// Directional operations supported by the host/plugin protocol.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type")]
pub enum MessageBody {
    /// Host opens a package-bound plugin session.
    #[serde(rename = "host.hello")]
    HostHello {
        /// Unique runtime session correlation identifier.
        session_id: String,
        /// Identity of the host implementation.
        host_id: String,
        /// Digest of the installed package the host intends to execute.
        package_digest: String,
    },
    /// Plugin confirms its identity and package digest.
    #[serde(rename = "plugin.ready")]
    PluginReady {
        /// Session identifier from [`MessageBody::HostHello`].
        session_id: String,
        /// Plugin identity declared by the executable.
        plugin_id: String,
        /// Package digest declared by the executable.
        package_digest: String,
    },
    /// Host invokes one exported plugin capability.
    #[serde(rename = "host.invoke")]
    HostInvoke {
        /// Active runtime session identifier.
        session_id: String,
        /// Unique invocation correlation identifier.
        invocation_id: String,
        /// Exported capability identifier.
        capability_id: String,
        /// Capability-specific structured input represented by the stable public API.
        input: Value,
    },
    /// Plugin completes an invocation.
    #[serde(rename = "plugin.result")]
    PluginResult {
        /// Active runtime session identifier.
        session_id: String,
        /// Invocation identifier from [`MessageBody::HostInvoke`].
        invocation_id: String,
        /// Successful structured output or a structured plugin error.
        outcome: WireOutcome,
    },
    /// Plugin asks the host to perform a permission-gated capability.
    #[serde(rename = "plugin.host_call")]
    PluginHostCall {
        /// Active runtime session identifier.
        session_id: String,
        /// Parent invocation that authorizes this host call.
        invocation_id: String,
        /// Unique host-call correlation identifier.
        call_id: String,
        /// Host capability identifier.
        capability_id: String,
        /// Capability-specific structured input represented by the stable public API.
        input: Value,
    },
    /// Host completes a plugin-originated host call.
    #[serde(rename = "host.host_result")]
    HostHostResult {
        /// Active runtime session identifier.
        session_id: String,
        /// Call identifier from [`MessageBody::PluginHostCall`].
        call_id: String,
        /// Successful structured output or a structured host error.
        outcome: WireOutcome,
    },
    /// Host requests cancellation of an in-flight invocation.
    #[serde(rename = "host.cancel")]
    HostCancel {
        /// Active runtime session identifier.
        session_id: String,
        /// Invocation identifier to cancel.
        invocation_id: String,
    },
    /// Host requests orderly plugin termination.
    #[serde(rename = "host.shutdown")]
    HostShutdown {
        /// Active runtime session identifier.
        session_id: String,
        /// Optional human-readable shutdown reason.
        reason: Option<String>,
    },
    /// Plugin confirms that its session has stopped.
    #[serde(rename = "plugin.stopped")]
    PluginStopped {
        /// Active runtime session identifier.
        session_id: String,
        /// Optional human-readable stop reason.
        reason: Option<String>,
    },
}

/// Result transported by invocation and host-call completion messages.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WireOutcome {
    /// Operation completed successfully.
    Succeeded {
        /// Operation-specific structured output.
        value: Value,
    },
    /// Operation failed without breaking the transport.
    Failed {
        /// Stable, machine-readable plugin error.
        error: PluginWireError,
    },
}

/// Structured error safe to serialize across the plugin process boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginWireError {
    /// Stable machine-readable error code.
    pub code: String,
    /// Human-readable explanation.
    pub message: String,
    /// Optional structured diagnostic context.
    pub details: Option<Value>,
    /// Whether retrying the same operation may succeed.
    pub retryable: bool,
}

impl PluginWireError {
    /// Creates a structured wire error without diagnostic details.
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
            retryable,
        }
    }
}

impl WireMessage {
    pub(crate) fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol.major != CURRENT_PROTOCOL_VERSION.major {
            return Err(ProtocolError::UnsupportedProtocolMajor {
                actual: self.protocol.major,
                expected: CURRENT_PROTOCOL_VERSION.major,
            });
        }
        self.body.validate()
    }
}

impl MessageBody {
    fn validate(&self) -> Result<(), ProtocolError> {
        validate_fields(self.message_type(), &[("session_id", self.session_id())])?;
        self.validate_handshake()?;
        self.validate_request()?;
        self.validate_completion()
    }

    fn validate_handshake(&self) -> Result<(), ProtocolError> {
        match self {
            Self::HostHello {
                host_id,
                package_digest,
                ..
            } => validate_host_hello(host_id, package_digest),
            Self::PluginReady {
                plugin_id,
                package_digest,
                ..
            } => validate_plugin_ready(plugin_id, package_digest),
            _ => Ok(()),
        }
    }

    fn validate_request(&self) -> Result<(), ProtocolError> {
        match self {
            Self::HostInvoke {
                invocation_id,
                capability_id,
                ..
            } => validate_invocation("host.invoke", "invocation_id", invocation_id, capability_id),
            Self::PluginHostCall {
                invocation_id,
                call_id,
                capability_id,
                ..
            } => {
                validate_fields("plugin.host_call", &[("invocation_id", invocation_id)])?;
                validate_invocation("plugin.host_call", "call_id", call_id, capability_id)
            }
            Self::HostCancel { invocation_id, .. } => {
                validate_fields("host.cancel", &[("invocation_id", invocation_id)])
            }
            _ => Ok(()),
        }
    }

    fn validate_completion(&self) -> Result<(), ProtocolError> {
        match self {
            Self::PluginResult { invocation_id, .. } => {
                validate_fields("plugin.result", &[("invocation_id", invocation_id)])
            }
            Self::HostHostResult { call_id, .. } => {
                validate_fields("host.host_result", &[("call_id", call_id)])
            }
            _ => Ok(()),
        }
    }

    fn message_type(&self) -> &'static str {
        match self {
            Self::HostHello { .. } => "host.hello",
            Self::PluginReady { .. } => "plugin.ready",
            Self::HostInvoke { .. } => "host.invoke",
            Self::PluginResult { .. } => "plugin.result",
            Self::PluginHostCall { .. } => "plugin.host_call",
            Self::HostHostResult { .. } => "host.host_result",
            Self::HostCancel { .. } => "host.cancel",
            Self::HostShutdown { .. } => "host.shutdown",
            Self::PluginStopped { .. } => "plugin.stopped",
        }
    }

    /// Returns the session identifier carried by every process-bound message.
    pub fn session_id(&self) -> &str {
        match self {
            Self::HostHello { session_id, .. }
            | Self::PluginReady { session_id, .. }
            | Self::HostInvoke { session_id, .. }
            | Self::PluginResult { session_id, .. }
            | Self::PluginHostCall { session_id, .. }
            | Self::HostHostResult { session_id, .. }
            | Self::HostCancel { session_id, .. }
            | Self::HostShutdown { session_id, .. }
            | Self::PluginStopped { session_id, .. } => session_id,
        }
    }
}

fn validate_host_hello(host_id: &str, package_digest: &str) -> Result<(), ProtocolError> {
    validate_fields(
        "host.hello",
        &[("host_id", host_id), ("package_digest", package_digest)],
    )
}

fn validate_plugin_ready(plugin_id: &str, package_digest: &str) -> Result<(), ProtocolError> {
    validate_fields(
        "plugin.ready",
        &[("plugin_id", plugin_id), ("package_digest", package_digest)],
    )
}

fn validate_invocation(
    message_type: &str,
    correlation_field: &str,
    correlation_id: &str,
    capability_id: &str,
) -> Result<(), ProtocolError> {
    validate_fields(
        message_type,
        &[
            (correlation_field, correlation_id),
            ("capability_id", capability_id),
        ],
    )
}

fn validate_fields(message_type: &str, fields: &[(&str, &str)]) -> Result<(), ProtocolError> {
    for (field, value) in fields {
        if value.trim().is_empty() {
            return Err(ProtocolError::MissingRequiredField {
                message_type: message_type.into(),
                field: (*field).into(),
                actual: value.to_string(),
                expected: "non-blank UTF-8 string".into(),
            });
        }
    }
    Ok(())
}
