use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct StreamControl {
    pub max_events: Option<usize>,
    deadline: Option<std::time::Instant>,
    cancel_flag: Option<Arc<AtomicBool>>,
    cancellation_token: Option<CancellationToken>,
}

impl StreamControl {
    /// Creates stream control without cancellation.
    pub fn unbounded() -> Self {
        Self {
            max_events: None,
            deadline: None,
            cancel_flag: None,
            cancellation_token: None,
        }
    }

    /// Creates stream control that cancels after a number of events.
    pub fn cancel_after(max_events: usize) -> Self {
        Self {
            max_events: Some(max_events),
            deadline: None,
            cancel_flag: None,
            cancellation_token: None,
        }
    }

    /// Bounds the work to an absolute deadline; `remaining()` reports time
    /// left (saturating to zero once passed).
    pub fn with_deadline(mut self, deadline: std::time::Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Time left before the deadline, or `None` when unbounded.
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()))
    }

    /// Adds an external cancellation flag shared with the caller.
    pub fn with_cancel_flag(mut self, cancel_flag: Arc<AtomicBool>) -> Self {
        self.cancel_flag = Some(cancel_flag);
        self
    }

    /// Shares a routing invocation cancellation token with streamed engine
    /// work; it introduces no independent deadline or cancellation model.
    pub fn from_cancellation_token(cancellation_token: CancellationToken) -> Self {
        Self {
            max_events: None,
            deadline: None,
            cancel_flag: None,
            cancellation_token: Some(cancellation_token),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag
            .as_ref()
            .map(|flag| flag.load(Ordering::SeqCst))
            .unwrap_or(false)
            || self
                .cancellation_token
                .as_ref()
                .map(CancellationToken::is_cancelled)
                .unwrap_or(false)
    }

    pub(crate) fn should_cancel_after(&self, event_count: usize) -> bool {
        self.is_cancelled()
            || self
                .max_events
                .map(|max_events| event_count >= max_events)
                .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::StreamControl;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn external_cancel_flag_requests_stream_cancellation() {
        let flag = Arc::new(AtomicBool::new(false));
        let control = StreamControl::unbounded().with_cancel_flag(Arc::clone(&flag));
        assert!(!control.should_cancel_after(0));
        flag.store(true, Ordering::SeqCst);
        assert!(control.should_cancel_after(0));
    }

    #[test]
    fn deadline_reports_remaining_and_saturates_at_zero() {
        let unbounded = StreamControl::unbounded();
        assert_eq!(unbounded.remaining(), None);
        let control =
            StreamControl::unbounded().with_deadline(Instant::now() + Duration::from_secs(60));
        let remaining = control.remaining().expect("deadline remaining");
        assert!(remaining <= Duration::from_secs(60) && remaining > Duration::ZERO);
        let expired = StreamControl::unbounded().with_deadline(Instant::now());
        assert_eq!(expired.remaining(), Some(Duration::ZERO));
    }
}
