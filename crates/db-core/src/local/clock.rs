use chrono::{DateTime, Utc};

/// Supplies wall-clock time to persistence rules that require deterministic tests.
pub trait Clock: Send + Sync {
    /// Returns the current UTC instant.
    ///
    /// # Example
    /// ```
    /// use lumvise_db_core::{Clock, SystemClock};
    /// let _now = SystemClock.now();
    /// ```
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
