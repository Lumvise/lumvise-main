use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Notify;

use crate::{PluginRuntimeError, SchemaDirection};

/// Scheduling class declared by a Plugin Invocation caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginInvocationClass {
    /// Interactive work whose result is awaited by a caller.
    Foreground,
    /// Durable or recurring work executed without blocking interactive callers.
    Background,
}

/// Cloneable cancellation handle shared with one Plugin Invocation context.
#[derive(Clone, Debug, Default)]
pub struct PluginInvocationCancellation {
    cancelled: Arc<AtomicBool>,
    wake: Arc<Notify>,
}

impl PluginInvocationCancellation {
    /// Creates an active cancellation handle.
    ///
    /// # Examples
    /// ```
    /// use lumvise_plugin_runtime::PluginInvocationCancellation;
    /// let cancellation = PluginInvocationCancellation::new();
    /// assert!(!cancellation.is_cancelled());
    /// cancellation.cancel();
    /// assert!(cancellation.is_cancelled());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks the shared invocation as cancelled and wakes async waiters.
    pub fn cancel(&self) {
        tracing::warn!(
            target: "debug_a4f2",
            "[DEBUG-a4f2] cancellation handle flipped to cancelled\n{}",
            std::backtrace::Backtrace::force_capture()
        );
        self.cancelled.store(true, Ordering::Release);
        self.wake.notify_waiters();
    }

    /// Returns whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(crate) async fn cancelled(&self) {
        if !self.is_cancelled() {
            self.wake.notified().await;
        }
    }
}

/// Caller-owned lifecycle information for one Plugin Invocation.
#[derive(Clone, Debug)]
pub struct PluginInvocationContext {
    request_id: String,
    owner_id: String,
    session_id: Option<String>,
    scope_id: Option<String>,
    class: PluginInvocationClass,
    created_at: Instant,
    deadline: Instant,
    cancellation: PluginInvocationCancellation,
}

impl PluginInvocationContext {
    /// Creates a context with an absolute monotonic deadline.
    ///
    /// # Examples
    /// ```
    /// use std::time::{Duration, Instant};
    /// use lumvise_plugin_runtime::{PluginInvocationClass, PluginInvocationContext};
    /// let context = PluginInvocationContext::new(
    ///     "request-1",
    ///     "mcp-owner-1",
    ///     PluginInvocationClass::Foreground,
    ///     Instant::now() + Duration::from_secs(1),
    /// );
    /// assert_eq!(context.request_id(), "request-1");
    /// ```
    pub fn new(
        request_id: impl Into<String>,
        owner_id: impl Into<String>,
        class: PluginInvocationClass,
        deadline: Instant,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            owner_id: owner_id.into(),
            session_id: None,
            scope_id: None,
            class,
            created_at: Instant::now(),
            deadline,
            cancellation: PluginInvocationCancellation::new(),
        }
    }

    /// Returns the caller's stable request correlation identity.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the caller or session owner identity.
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// Returns the optional caller transport session identity.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Returns the optional signed route scope identity.
    pub fn scope_id(&self) -> Option<&str> {
        self.scope_id.as_deref()
    }

    /// Adds transport-neutral session and optional scope correlation.
    pub fn with_route_identity(
        mut self,
        session_id: impl Into<String>,
        scope_id: Option<String>,
    ) -> Self {
        self.session_id = Some(session_id.into());
        self.scope_id = scope_id;
        self
    }

    /// Returns the declared scheduling class.
    pub fn class(&self) -> PluginInvocationClass {
        self.class
    }

    /// Returns the caller's absolute monotonic deadline.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Returns a handle that can cancel this invocation from another thread.
    pub fn cancellation(&self) -> PluginInvocationCancellation {
        self.cancellation.clone()
    }

    #[cfg(test)]
    pub(crate) fn with_timeout(
        request_id: impl Into<String>,
        owner_id: impl Into<String>,
        class: PluginInvocationClass,
        timeout: Duration,
    ) -> Self {
        Self::new(request_id, owner_id, class, Instant::now() + timeout)
    }

    pub(crate) fn with_deadline_cap(mut self, deadline: Instant) -> Self {
        self.deadline = self.deadline.min(deadline);
        self
    }

    pub(crate) fn ensure_active(
        &self,
        plugin_id: &str,
        maximum: Duration,
    ) -> Result<Duration, PluginRuntimeError> {
        self.validate()?;
        if self.cancellation.is_cancelled() {
            tracing::warn!(target: "debug_a4f2", plugin_id,
                request_id = %self.request_id,
                "[DEBUG-a4f2] invocation admission saw cancelled handle");
            return Err(PluginRuntimeError::InvocationCancelled {
                plugin_id: plugin_id.to_owned(),
                request_id: self.request_id.clone(),
            });
        }
        self.remaining(plugin_id, maximum)
    }

    fn validate(&self) -> Result<(), PluginRuntimeError> {
        require_identity("request_id", &self.request_id)?;
        require_identity("owner_id", &self.owner_id)?;
        if let Some(session_id) = &self.session_id {
            require_identity("session_id", session_id)?;
        }
        if let Some(scope_id) = &self.scope_id {
            require_identity("scope_id", scope_id)?;
        }
        Ok(())
    }

    fn remaining(
        &self,
        plugin_id: &str,
        maximum: Duration,
    ) -> Result<Duration, PluginRuntimeError> {
        let deadline = self.effective_deadline(maximum);
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            return Ok(remaining);
        }
        Err(PluginRuntimeError::InvocationTimeout {
            plugin_id: plugin_id.to_owned(),
            invocation_id: self.request_id.clone(),
            deadline: deadline.saturating_duration_since(self.created_at),
            stderr: "process was not dispatched before the invocation deadline".to_owned(),
        })
    }

    pub(crate) fn effective_deadline(&self, maximum: Duration) -> Instant {
        let runtime_deadline = self
            .created_at
            .checked_add(maximum)
            .unwrap_or(self.deadline);
        self.deadline.min(runtime_deadline)
    }
}

/// One ready Plugin export call plus its lifecycle context.
#[derive(Debug)]
pub struct PluginInvocationRequest {
    plugin_id: String,
    export_id: String,
    input: Value,
    context: PluginInvocationContext,
}

/// Exact identity tuple authorized to cancel one controlled invocation.
#[derive(Clone, Debug)]
pub struct PluginInvocationCancellationRequest {
    /// Target Plugin identity when known at the cancelling boundary.
    pub plugin_id: Option<String>,
    /// Caller correlation identity.
    pub request_id: String,
    /// Owning caller identity.
    pub owner_id: String,
    /// Optional transport session identity.
    pub session_id: Option<String>,
    /// Optional signed route scope identity.
    pub scope_id: Option<String>,
}

impl PluginInvocationRequest {
    /// Creates a controlled Plugin Invocation request.
    pub fn new(
        plugin_id: impl Into<String>,
        export_id: impl Into<String>,
        input: Value,
        context: PluginInvocationContext,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            export_id: export_id.into(),
            input,
            context,
        }
    }

    /// Returns the signed Plugin identity.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Returns the signed export identity.
    pub fn export_id(&self) -> &str {
        &self.export_id
    }

    /// Returns the serialized Plugin input.
    pub fn input(&self) -> &Value {
        &self.input
    }

    /// Returns the caller-owned lifecycle context.
    pub fn context(&self) -> &PluginInvocationContext {
        &self.context
    }

    pub(crate) fn into_parts(self) -> (String, String, Value, PluginInvocationContext) {
        (self.plugin_id, self.export_id, self.input, self.context)
    }
}

/// Stable failure category exposed to Plugin Invocation callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginInvocationFailureKind {
    /// The target, input, or lifecycle context is invalid.
    InvalidInput,
    /// The target Plugin's bounded admission queue is full.
    Busy,
    /// The absolute invocation deadline elapsed.
    DeadlineExceeded,
    /// The caller or Runtime cancelled the invocation.
    Cancelled,
    /// The target is known but temporarily unavailable.
    Unavailable,
    /// The Plugin or Plugin Protocol violated its declared contract.
    PluginFailure,
    /// Host-owned Runtime state failed unexpectedly.
    Internal,
}

/// Typed Plugin Invocation failure retaining the detailed Runtime source.
#[derive(Debug, thiserror::Error)]
#[error("plugin invocation failed with {kind:?}: {source}")]
pub struct PluginInvocationError {
    kind: PluginInvocationFailureKind,
    #[source]
    source: PluginRuntimeError,
}

impl PluginInvocationError {
    /// Returns the stable caller-facing failure category.
    pub fn kind(&self) -> PluginInvocationFailureKind {
        self.kind
    }

    /// Returns whether retrying later can succeed without changing the request.
    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            PluginInvocationFailureKind::Busy
                | PluginInvocationFailureKind::DeadlineExceeded
                | PluginInvocationFailureKind::Unavailable
        )
    }

    /// Returns the detailed Runtime failure.
    pub fn runtime_error(&self) -> &PluginRuntimeError {
        &self.source
    }

    pub(crate) fn from_runtime(source: PluginRuntimeError) -> Self {
        Self {
            kind: failure_kind(&source),
            source,
        }
    }

    /// Consumes the typed wrapper and returns the detailed Runtime error.
    pub fn into_runtime_error(self) -> PluginRuntimeError {
        self.source
    }
}

fn require_identity(field: &'static str, value: &str) -> Result<(), PluginRuntimeError> {
    if !value.trim().is_empty() {
        return Ok(());
    }
    Err(PluginRuntimeError::InvocationContextInvalid {
        field,
        value: value.to_owned(),
        expected: "a non-empty identity",
    })
}

fn failure_kind(error: &PluginRuntimeError) -> PluginInvocationFailureKind {
    use PluginInvocationFailureKind as Kind;
    if is_invalid_invocation(error) {
        Kind::InvalidInput
    } else if is_unavailable_invocation(error) {
        Kind::Unavailable
    } else if is_plugin_failure(error) {
        Kind::PluginFailure
    } else {
        match error {
            PluginRuntimeError::InvocationBusy { .. } => Kind::Busy,
            PluginRuntimeError::InvocationTimeout { .. }
            | PluginRuntimeError::ExclusiveLaneTimeout { .. } => Kind::DeadlineExceeded,
            PluginRuntimeError::InvocationCancelled { .. }
            | PluginRuntimeError::ExclusiveLaneCancelled { .. } => Kind::Cancelled,
            _ => Kind::Internal,
        }
    }
}

fn is_invalid_invocation(error: &PluginRuntimeError) -> bool {
    matches!(
        error,
        PluginRuntimeError::InvocationContextInvalid { .. }
            | PluginRuntimeError::InvocationIdentityConflict { .. }
            | PluginRuntimeError::NotInstalled(_)
            | PluginRuntimeError::UndeclaredExport { .. }
            | PluginRuntimeError::InvalidCommandSurface { .. }
            | PluginRuntimeError::ViewNotFound { .. }
            | PluginRuntimeError::ViewHostApiDenied { .. }
            | PluginRuntimeError::ExclusiveLaneDenied { .. }
            | PluginRuntimeError::SchemaValidation {
                direction: SchemaDirection::Input,
                ..
            }
    )
}

fn is_unavailable_invocation(error: &PluginRuntimeError) -> bool {
    matches!(
        error,
        PluginRuntimeError::Sandbox { .. }
            | PluginRuntimeError::Spawn { .. }
            | PluginRuntimeError::ProcessExited { .. }
            | PluginRuntimeError::HandshakeTimeout { .. }
            | PluginRuntimeError::NotReady(_)
    )
}

fn is_plugin_failure(error: &PluginRuntimeError) -> bool {
    matches!(
        error,
        PluginRuntimeError::Protocol { .. }
            | PluginRuntimeError::CorrelationMismatch { .. }
            | PluginRuntimeError::HostCallParentMismatch { .. }
            | PluginRuntimeError::ReentrantHostCall { .. }
            | PluginRuntimeError::InvocationCycle { .. }
            | PluginRuntimeError::MessageSessionMismatch { .. }
            | PluginRuntimeError::ViewHostApiFailed { .. }
            | PluginRuntimeError::ViewAssetInvalid { .. }
            | PluginRuntimeError::SchemaValidation {
                direction: SchemaDirection::Output,
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_exposes_identity_class_deadline_and_shared_cancellation() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let context = PluginInvocationContext::new(
            "request-1",
            "owner-1",
            PluginInvocationClass::Background,
            deadline,
        );
        let cancellation = context.cancellation();

        cancellation.cancel();

        assert_eq!(context.request_id(), "request-1");
        assert_eq!(context.owner_id(), "owner-1");
        assert_eq!(context.class(), PluginInvocationClass::Background);
        assert_eq!(context.deadline(), deadline);
        assert!(context.cancellation().is_cancelled());
    }

    #[test]
    fn request_exposes_target_input_and_context() {
        let context = PluginInvocationContext::with_timeout(
            "request-2",
            "owner-2",
            PluginInvocationClass::Foreground,
            Duration::from_secs(1),
        );
        let request = PluginInvocationRequest::new(
            "plugin.test",
            "test.echo",
            serde_json::json!({"value": 1}),
            context,
        );

        assert_eq!(request.plugin_id(), "plugin.test");
        assert_eq!(request.export_id(), "test.echo");
        assert_eq!(request.input(), &serde_json::json!({"value": 1}));
        assert_eq!(request.context().owner_id(), "owner-2");
    }

    #[test]
    fn typed_error_classifies_retryable_and_terminal_runtime_failures() {
        let busy = PluginInvocationError::from_runtime(PluginRuntimeError::InvocationBusy {
            plugin_id: "plugin.busy".into(),
            active: 1,
            queued: 8,
        });
        let invalid =
            PluginInvocationError::from_runtime(PluginRuntimeError::InvocationContextInvalid {
                field: "owner_id",
                value: "".into(),
                expected: "a non-empty identity",
            });

        assert_eq!(busy.kind(), PluginInvocationFailureKind::Busy);
        assert!(busy.retryable());
        assert_eq!(invalid.kind(), PluginInvocationFailureKind::InvalidInput);
        assert!(!invalid.retryable());
        assert!(matches!(
            invalid.runtime_error(),
            PluginRuntimeError::InvocationContextInvalid {
                field: "owner_id",
                ..
            }
        ));
    }
}
