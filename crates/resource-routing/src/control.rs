use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

pub trait InvocationClock: Send + Sync {
    fn monotonic_now(&self) -> Instant;
    fn unix_ms_now(&self) -> u64;
}

#[derive(Default)]
pub struct SystemInvocationClock;

impl InvocationClock for SystemInvocationClock {
    fn monotonic_now(&self) -> Instant {
        Instant::now()
    }
    fn unix_ms_now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock predates Unix epoch")
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

/// One invocation's end-to-end deadline and cancellation signal.
///
/// `deadline` is the private monotonic deadline used locally. Its paired,
/// immutable wall-clock projection is serialized in `InvocationStartV1` and
/// readiness. Children share both values and cancellation exactly.
#[derive(Clone, Debug)]
pub struct InvocationControl {
    deadline: Instant,
    deadline_unix_ms: u64,
    cancellation: CancellationToken,
}

impl InvocationControl {
    pub const DEADLINE: Duration = Duration::from_secs(60);

    pub fn sixty_seconds() -> Self {
        Self::with_deadline(Self::DEADLINE)
    }

    pub fn with_clock(clock: &dyn InvocationClock) -> Self {
        Self::with_deadline_and_clock(Self::DEADLINE, clock)
    }

    /// Budgets one invocation for a caller-chosen duration instead of the
    /// default sixty-second foreground ceiling. Background work (e.g. a
    /// dispatched LLM job explicitly decoupled from any single plugin
    /// invocation) must not reuse `sixty_seconds()`: that ceiling is a
    /// foreground-invocation policy, not a bound on how long legitimate
    /// background work may run.
    pub fn with_deadline(deadline: Duration) -> Self {
        Self::with_deadline_and_clock(deadline, &SystemInvocationClock)
    }

    pub fn with_deadline_and_clock(deadline: Duration, clock: &dyn InvocationClock) -> Self {
        Self {
            deadline: clock.monotonic_now() + deadline,
            deadline_unix_ms: clock
                .unix_ms_now()
                .saturating_add(deadline.as_millis().try_into().unwrap_or(u64::MAX)),
            cancellation: CancellationToken::new(),
        }
    }

    /// Reconstitutes a peer's absolute wall-clock deadline without creating a
    /// new phase budget. Transport endpoints must reject implausibly distant
    /// deadlines before calling this method.
    pub fn from_deadline_unix_ms(deadline_unix_ms: u64) -> Self {
        let now_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock predates Unix epoch")
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        Self {
            deadline: Instant::now()
                + Duration::from_millis(deadline_unix_ms.saturating_sub(now_unix_ms)),
            deadline_unix_ms,
            cancellation: CancellationToken::new(),
        }
    }

    pub fn child(&self) -> Self {
        self.clone()
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn deadline_unix_ms(&self) -> u64 {
        self.deadline_unix_ms
    }
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
    pub fn is_expired(&self) -> bool {
        self.remaining().is_zero()
    }
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}
