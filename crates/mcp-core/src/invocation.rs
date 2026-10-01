use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Cloneable cancellation signal for one MCP request lifecycle.
#[derive(Clone, Debug, Default)]
pub struct McpInvocationCancellation {
    cancelled: Arc<AtomicBool>,
}

impl McpInvocationCancellation {
    /// Creates an active cancellation signal.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation across every downstream boundary.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Returns whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Borrows the shared flag for adapters that must pass cancellation across
    /// a synchronous process boundary.
    pub fn as_atomic(&self) -> &AtomicBool {
        self.cancelled.as_ref()
    }
}

/// Transport-neutral identity and absolute deadline for one MCP invocation.
#[derive(Clone, Debug)]
pub struct McpInvocationContext {
    request_id: String,
    owner_id: String,
    session_id: String,
    scope_id: Option<String>,
    deadline_unix_ms: u64,
    cancellation: McpInvocationCancellation,
}

impl McpInvocationContext {
    /// Creates one ingress context with an absolute Unix-epoch deadline.
    pub fn new(
        request_id: impl Into<String>,
        owner_id: impl Into<String>,
        session_id: impl Into<String>,
        scope_id: Option<String>,
        deadline_unix_ms: u64,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            owner_id: owner_id.into(),
            session_id: session_id.into(),
            scope_id,
            deadline_unix_ms,
            cancellation: McpInvocationCancellation::new(),
        }
    }

    /// Creates one ingress context whose deadline is relative to now.
    pub fn with_timeout(
        request_id: impl Into<String>,
        owner_id: impl Into<String>,
        session_id: impl Into<String>,
        scope_id: Option<String>,
        timeout: Duration,
    ) -> Self {
        Self::new(
            request_id,
            owner_id,
            session_id,
            scope_id,
            deadline_after(timeout),
        )
    }

    /// Returns the JSON-RPC correlation identity.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns the stable MCP owner identity.
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    /// Returns the transport session identity.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the optional scoped MCP route identity.
    pub fn scope_id(&self) -> Option<&str> {
        self.scope_id.as_deref()
    }

    /// Returns the absolute Unix-epoch deadline in milliseconds.
    pub fn deadline_unix_ms(&self) -> u64 {
        self.deadline_unix_ms
    }

    /// Returns remaining wall-clock time without extending the ingress deadline.
    pub fn remaining(&self) -> Duration {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let remaining = u128::from(self.deadline_unix_ms).saturating_sub(now);
        Duration::from_millis(remaining.try_into().unwrap_or(u64::MAX))
    }

    /// Returns the shared cancellation signal.
    pub fn cancellation(&self) -> McpInvocationCancellation {
        self.cancellation.clone()
    }
}

fn deadline_after(timeout: Duration) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .saturating_add(timeout)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_preserves_ingress_identity_deadline_and_cancellation() {
        let context = McpInvocationContext::with_timeout(
            "request-1",
            "owner-1",
            "session-1",
            Some("scope-1".into()),
            Duration::from_secs(1),
        );
        let cancellation = context.cancellation();
        cancellation.cancel();

        assert_eq!(context.request_id(), "request-1");
        assert_eq!(context.owner_id(), "owner-1");
        assert_eq!(context.session_id(), "session-1");
        assert_eq!(context.scope_id(), Some("scope-1"));
        assert!(context.deadline_unix_ms() > 0);
        assert!(context.cancellation().is_cancelled());
    }
}
