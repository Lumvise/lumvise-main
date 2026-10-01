use std::{io, path::PathBuf, time::Duration};

/// Signed schema side involved in compilation or instance validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaDirection {
    /// Host-to-plugin invocation input.
    Input,
    /// Plugin-to-host successful output.
    Output,
}

impl std::fmt::Display for SchemaDirection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input => formatter.write_str("input"),
            Self::Output => formatter.write_str("output"),
        }
    }
}

/// Failures at the compiled plugin lifecycle boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginRuntimeError {
    /// The configured OS sandbox denied or failed process preparation.
    #[error("plugin `{plugin_id}` sandbox preparation failed: {source}")]
    Sandbox {
        /// Plugin denied execution.
        plugin_id: String,
        /// Sandbox policy or adapter failure.
        source: crate::PluginSandboxError,
    },
    /// A signed export schema could not compile as Draft 2020-12.
    #[error(
        "plugin `{plugin_id}` export `{export_id}` {direction} schema failed Draft 2020-12 compilation: {message}"
    )]
    SchemaCompilation {
        /// Signed plugin identity.
        plugin_id: String,
        /// Signed export identity.
        export_id: String,
        /// Input or output schema.
        direction: SchemaDirection,
        /// Validator compilation diagnostic.
        message: String,
    },
    /// An invocation instance violated its signed export schema.
    #[error(
        "plugin `{plugin_id}` export `{export_id}` {direction} instance at `{instance_path}` violates signed schema: {message}"
    )]
    SchemaValidation {
        /// Signed plugin identity.
        plugin_id: String,
        /// Signed export identity.
        export_id: String,
        /// Input or output schema.
        direction: SchemaDirection,
        /// RFC 6901 JSON Pointer into the offending instance.
        instance_path: String,
        /// Validator diagnostic.
        message: String,
    },
    /// Another package with this identity is already cataloged.
    #[error("plugin `{0}` is already installed; expected a unique plugin id")]
    AlreadyInstalled(String),
    /// No installed package has the requested identity.
    #[error("plugin `{0}` is not installed; expected an installed plugin id")]
    NotInstalled(String),
    /// The plugin process already completed its handshake.
    #[error("plugin `{0}` is already active; expected a stopped plugin")]
    AlreadyActive(String),
    /// Invocation was attempted before a successful ready handshake.
    #[error("plugin `{0}` is not ready; expected start to complete before invoke")]
    NotReady(String),
    /// Active plugins must stop before removal from the catalog.
    #[error("plugin `{0}` is active; expected stop before uninstall")]
    ActiveUninstall(String),
    /// The operating system could not launch the package executable.
    #[error("failed to spawn plugin executable `{path}`: {source}")]
    Spawn {
        /// Offending executable path.
        path: PathBuf,
        /// Operating-system failure.
        source: io::Error,
    },
    /// A framed protocol operation failed.
    #[error("plugin `{plugin_id}` protocol failed: {message}; stderr: {stderr}")]
    Protocol {
        /// Plugin involved in the protocol failure.
        plugin_id: String,
        /// Codec or transport message.
        message: String,
        /// Captured child diagnostics.
        stderr: String,
    },
    /// The plugin exited while the host awaited a protocol message.
    #[error("plugin `{plugin_id}` exited with status `{status}`; stderr: {stderr}")]
    ProcessExited {
        /// Plugin whose child process ended.
        plugin_id: String,
        /// Platform process status.
        status: String,
        /// Captured child diagnostics.
        stderr: String,
    },
    /// The plugin did not become ready within its deadline.
    #[error("plugin `{plugin_id}` handshake timed out after {deadline:?}; stderr: {stderr}")]
    HandshakeTimeout {
        /// Plugin waiting for readiness.
        plugin_id: String,
        /// Configured handshake deadline.
        deadline: Duration,
        /// Captured child diagnostics.
        stderr: String,
    },
    /// Ready identity was not the installed signed package identity.
    #[error(
        "plugin ready identity mismatch: expected id `{expected_id}` digest `{expected_digest}`, got id `{actual_id}` digest `{actual_digest}`"
    )]
    ReadyIdentityMismatch {
        /// Installed plugin id.
        expected_id: String,
        /// Installed signed package digest.
        expected_digest: String,
        /// Process-declared plugin id.
        actual_id: String,
        /// Process-declared package digest.
        actual_digest: String,
    },
    /// Ready message belonged to a different runtime session.
    #[error("plugin `{plugin_id}` ready session mismatch: expected `{expected}`, got `{actual}`")]
    ReadySessionMismatch {
        /// Plugin returning readiness.
        plugin_id: String,
        /// Host-created session id.
        expected: String,
        /// Process-returned session id.
        actual: String,
    },
    /// An invocation result did not match its request.
    #[error("plugin `{plugin_id}` returned invocation `{actual}`; expected `{expected}`")]
    CorrelationMismatch {
        /// Plugin returning the result.
        plugin_id: String,
        /// Requested invocation id.
        expected: String,
        /// Returned invocation id.
        actual: String,
    },
    /// A host call named a different parent invocation.
    #[error(
        "plugin `{plugin_id}` host call parent mismatch: expected `{expected}`, got `{actual}`"
    )]
    HostCallParentMismatch {
        /// Plugin returning the host call.
        plugin_id: String,
        /// Live parent invocation id.
        expected: String,
        /// Plugin-declared parent invocation id.
        actual: String,
    },
    /// A host call correlation id was already used by the live invocation.
    #[error("plugin `{plugin_id}` repeated host call `{call_id}`; expected unique call ids")]
    ReentrantHostCall {
        /// Plugin returning the duplicate call.
        plugin_id: String,
        /// Repeated call correlation id.
        call_id: String,
    },
    /// A nested plugin invocation repeated an identity already on its call stack.
    #[error("plugin invocation cycle `{path}`; expected each nested plugin identity once")]
    InvocationCycle {
        /// Deterministic caller-to-target cycle path.
        path: String,
    },
    /// Invocation targeted an identity absent from the signed export catalog.
    #[error("plugin `{plugin_id}` does not export `{capability_id}`; expected a signed export id")]
    UndeclaredExport {
        /// Invoked plugin.
        plugin_id: String,
        /// Undeclared export identity.
        capability_id: String,
    },
    /// Host attempted Command dispatch through an export signed for another surface.
    #[error(
        "plugin `{plugin_id}` export `{export_id}` is not a Command; expected signed Command surface"
    )]
    InvalidCommandSurface {
        /// Plugin identity.
        plugin_id: String,
        /// Export identity signed for a different surface.
        export_id: String,
    },
    /// Ready plugin has no signed View with requested identity.
    #[error("plugin `{plugin_id}` has no signed View `{view_id}`; expected declared ready View")]
    ViewNotFound {
        /// Plugin identity.
        plugin_id: String,
        /// Requested View identity.
        view_id: String,
    },
    /// View requested a Host API absent from its signed allowlist or capability declarations.
    #[error(
        "plugin `{plugin_id}` View `{view_id}` cannot call Host API `{api_id}`; expected signed View allowlist and Host Capability declaration"
    )]
    ViewHostApiDenied {
        /// Plugin identity.
        plugin_id: String,
        /// View identity.
        view_id: String,
        /// Requested Host API.
        api_id: String,
    },
    /// View Host API failed at host policy or adapter execution.
    #[error("plugin `{plugin_id}` View `{view_id}` Host API `{api_id}` failed: {message}")]
    ViewHostApiFailed {
        /// Plugin identity.
        plugin_id: String,
        /// View identity.
        view_id: String,
        /// Requested Host API.
        api_id: String,
        /// Structured broker diagnostic.
        message: String,
    },
    /// Signed View asset could not be read or no longer matches its signed digest.
    #[error("plugin `{plugin_id}` View `{view_id}` asset `{path}` failed verification: {message}")]
    ViewAssetInvalid {
        /// Plugin identity.
        plugin_id: String,
        /// View identity.
        view_id: String,
        /// Signed package-relative path.
        path: String,
        /// Read or digest diagnostic.
        message: String,
    },
    /// A message belonged to another active process session.
    #[error("plugin `{plugin_id}` message session mismatch: expected `{expected}`, got `{actual}`")]
    MessageSessionMismatch {
        /// Plugin returning the message.
        plugin_id: String,
        /// Active process session id.
        expected: String,
        /// Returned message session id.
        actual: String,
    },
    /// Invocation exceeded the configured deadline.
    #[error(
        "plugin `{plugin_id}` invocation `{invocation_id}` timed out after {deadline:?}; stderr: {stderr}"
    )]
    InvocationTimeout {
        /// Plugin that exceeded its deadline.
        plugin_id: String,
        /// Correlation id of the timed-out invocation.
        invocation_id: String,
        /// Configured invocation deadline.
        deadline: Duration,
        /// Captured child diagnostics.
        stderr: String,
    },
    /// Caller-owned Plugin Invocation lifecycle data is malformed.
    #[error("plugin invocation context field `{field}` was `{value}`; expected {expected}")]
    InvocationContextInvalid {
        /// Invalid context field name.
        field: &'static str,
        /// Offending caller-provided value.
        value: String,
        /// Required field shape.
        expected: &'static str,
    },
    /// One owner reused a live request correlation identity.
    #[error(
        "plugin invocation owner `{owner_id}` reused live request `{request_id}`; expected unique active correlation identity"
    )]
    InvocationIdentityConflict {
        /// Conflicting owner identity.
        owner_id: String,
        /// Conflicting request identity.
        request_id: String,
    },
    /// The target Plugin's bounded admission queue cannot accept more work.
    #[error(
        "plugin `{plugin_id}` invocation admission is busy with active `{active}` and queued `{queued}`; expected available plugin-local capacity"
    )]
    InvocationBusy {
        /// Saturated Plugin identity.
        plugin_id: String,
        /// Number of actively executing invocations.
        active: usize,
        /// Number of queued invocations.
        queued: usize,
    },
    /// The caller cancelled an invocation before normal completion.
    #[error("plugin `{plugin_id}` invocation request `{request_id}` was cancelled")]
    InvocationCancelled {
        /// Target Plugin identity.
        plugin_id: String,
        /// Caller-owned request correlation identity.
        request_id: String,
    },
    /// Shared Plugin Invocation admission state became unavailable.
    #[error("plugin invocation admission lock is poisoned")]
    InvocationAdmissionPoisoned,
    /// Signed exclusive-lane metadata is invalid at runtime.
    #[error(
        "plugin `{plugin_id}` export `{export_id}` has invalid exclusive-lane policy: {message}"
    )]
    ExclusiveLaneInvalid {
        /// Plugin declaring the policy.
        plugin_id: String,
        /// Export declaring the policy.
        export_id: String,
        /// Invalid signed policy detail.
        message: String,
    },
    /// One exclusive-lane admission request was denied.
    #[error("exclusive lane `{lane_id}` rejected `{value}`; expected {expected}")]
    ExclusiveLaneDenied {
        /// Signed lane identity.
        lane_id: String,
        /// Rejected input or state.
        value: String,
        /// Accepted state or value.
        expected: String,
    },
    /// One exclusive-lane waiter exceeded its signed deadline.
    #[error("exclusive lane `{lane_id}` admission timed out")]
    ExclusiveLaneTimeout {
        /// Signed lane identity.
        lane_id: String,
    },
    /// One exclusive-lane waiter was cancelled by plugin lifecycle teardown.
    #[error("plugin `{plugin_id}` exclusive lane `{lane_id}` admission was cancelled")]
    ExclusiveLaneCancelled {
        /// Plugin whose lifecycle ended.
        plugin_id: String,
        /// Signed lane identity.
        lane_id: String,
    },
    /// Shared exclusive-lane state became unavailable.
    #[error("exclusive invocation lane lock is poisoned")]
    ExclusiveLanePoisoned,
    /// Shared lifecycle state became unavailable.
    #[error("plugin runtime catalog lock is poisoned; expected synchronized lifecycle access")]
    CatalogPoisoned,
    /// One plugin's lifecycle state became unavailable.
    #[error("plugin `{0}` lifecycle lock is poisoned; expected isolated lifecycle access")]
    PluginStatePoisoned(String),
}
