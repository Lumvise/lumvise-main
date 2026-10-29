use serde_json::Value;

use crate::PluginInvocationContext;

/// One permission-gated operation requested by a plugin during an invocation.
#[derive(Clone, Debug)]
pub struct HostCapabilityRequest {
    /// Calling plugin identity.
    pub plugin_id: String,
    /// Live parent plugin invocation.
    pub invocation_id: String,
    /// Host-call correlation identity.
    pub call_id: String,
    /// Requested Host Capability identity.
    pub capability_id: String,
    /// Signed semantic version requirement for the capability.
    pub required_version: String,
    /// Capability-specific JSON input.
    pub input: Value,
}

/// A host service's fixed publication target, executed inside its caller's invocation.
#[derive(Clone, Debug)]
pub struct HostPluginInvocation {
    /// Target plugin selected by the host service, never by an import request.
    pub plugin_id: String,
    /// Target export selected by the host service.
    pub export_id: String,
    /// Target export input.
    pub input: Value,
}

/// Runtime-owned nested access with inherited identity, cancellation and deadline.
///
/// The result is the raw target export output, without a `plugin.invoke` envelope.
/// Example: `nested.invoke_plugin(HostPluginInvocation { plugin_id, export_id, input })`.
pub trait HostPluginInvoker: Send + Sync {
    /// Executes one authorized nested export without registering another root invocation.
    fn invoke_plugin(&self, request: HostPluginInvocation) -> Result<Value, HostCapabilityError>;
}

/// Policy or execution failure returned to a requesting plugin.
#[derive(Clone, Debug, thiserror::Error)]
#[error("Host Capability `{capability_id}` failed with `{code}`: {message}")]
pub struct HostCapabilityError {
    capability_id: String,
    code: String,
    message: String,
    details: Option<Value>,
    retryable: bool,
}

impl HostCapabilityError {
    /// Creates a structured failure safe to return over the plugin protocol.
    pub fn new(
        capability_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            capability_id: capability_id.into(),
            code: code.into(),
            message: message.into(),
            details: None,
            retryable,
        }
    }

    /// Attaches structured diagnostics to the failure.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub(crate) fn into_wire_error(self) -> lumvise_plugin_protocol::PluginWireError {
        lumvise_plugin_protocol::PluginWireError {
            code: self.code,
            message: self.message,
            details: self.details,
            retryable: self.retryable,
        }
    }
}

/// Host-owned policy and execution boundary for privileged plugin operations.
pub trait HostCapabilityBroker: Send + Sync {
    /// Applies host grants and executes an allowed capability.
    fn invoke(&self, request: HostCapabilityRequest) -> Result<Value, HostCapabilityError>;

    /// Executes with the parent invocation lifecycle without owning scheduling.
    fn invoke_controlled(
        &self,
        request: HostCapabilityRequest,
        _context: &PluginInvocationContext,
    ) -> Result<Value, HostCapabilityError> {
        self.invoke(request)
    }

    /// Executes a service with runtime-owned nested access for this host call only.
    ///
    /// Example: an importer publishes through `nested` under the original caller.
    fn invoke_with_nested(
        &self,
        request: HostCapabilityRequest,
        context: &PluginInvocationContext,
        _nested: &dyn HostPluginInvoker,
    ) -> Result<Value, HostCapabilityError> {
        self.invoke_controlled(request, context)
    }
}

/// Named production-safe broker that grants no Host Capabilities.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAllHostCapabilityBroker;

impl HostCapabilityBroker for DenyAllHostCapabilityBroker {
    fn invoke(&self, request: HostCapabilityRequest) -> Result<Value, HostCapabilityError> {
        Err(HostCapabilityError::new(
            request.capability_id,
            "host_capability_denied",
            "host policy grants no capabilities",
            false,
        ))
    }
}
