use serde_json::Value;

use crate::PluginError;

/// Transport one `PluginContext` host call through the owning runtime.
///
/// Exposed only with the `test-support` feature so plugin crates can back a
/// context with an in-process fake host from their own unit tests.
#[cfg(feature = "test-support")]
pub trait HostCallTransport {
    /// Executes one permission-gated host capability call.
    ///
    /// # Errors
    /// Returns the host's structured failure for the call.
    fn host_call(&mut self, capability_id: &str, input: Value) -> Result<Value, PluginError>;
}

#[cfg(not(feature = "test-support"))]
pub(crate) trait HostCallTransport {
    fn host_call(&mut self, capability_id: &str, input: Value) -> Result<Value, PluginError>;
}

/// Invocation-scoped access to permission-gated capabilities provided by the host.
pub struct PluginContext<'a> {
    transport: &'a mut dyn HostCallTransport,
}

impl<'a> PluginContext<'a> {
    pub(crate) fn new(transport: &'a mut dyn HostCallTransport) -> Self {
        Self { transport }
    }

    /// Creates a context over a caller-supplied transport. Available only with
    /// the `test-support` feature; production contexts come from the session.
    #[cfg(feature = "test-support")]
    pub fn for_test(transport: &'a mut dyn HostCallTransport) -> Self {
        Self { transport }
    }

    /// Requests one permission-gated host capability and waits for its correlated result.
    ///
    /// # Errors
    ///
    /// Returns a structured host failure, cancellation, or degraded-protocol error. The host
    /// remains the authority that decides whether the requested capability is permitted.
    pub fn host_call(&mut self, capability_id: &str, input: Value) -> Result<Value, PluginError> {
        self.transport.host_call(capability_id, input)
    }
}
