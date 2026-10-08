use chrono::{DateTime, Utc};

/// Supplies wall-clock time to persistence rules that require deterministic tests.
pub trait Clock: Send + Sync {
    /// Returns the current UTC instant.
    ///
    /// This clock is injected by the private runtime for deterministic persistence
    /// tests. `SystemClock` delegates to `chrono::Utc::now`; callers use persistence
    /// operations and do not access the clock.
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock backed by the host system clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
