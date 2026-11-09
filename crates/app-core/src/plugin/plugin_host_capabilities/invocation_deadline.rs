//! Captures one clock sample so forwarding never adds constructor latency to a deadline.
use lumvise_resource_routing::{
    InvocationControl,
    control::{InvocationClock, SystemInvocationClock},
};
use std::time::Instant;

struct PluginDeadlineClock {
    monotonic: Instant,
    unix_ms: u64,
}

impl InvocationClock for PluginDeadlineClock {
    fn monotonic_now(&self) -> Instant {
        self.monotonic
    }
    fn unix_ms_now(&self) -> u64 {
        self.unix_ms
    }
}

pub(super) fn from_parent(deadline: Instant) -> InvocationControl {
    let observed = Instant::now();
    let elapsed = observed.saturating_duration_since(deadline);
    let clock = PluginDeadlineClock {
        monotonic: observed.min(deadline),
        unix_ms: SystemInvocationClock
            .unix_ms_now()
            .saturating_sub(elapsed.as_millis().try_into().unwrap_or(u64::MAX)),
    };
    let remaining = deadline
        .saturating_duration_since(clock.monotonic)
        .min(InvocationControl::DEADLINE);
    InvocationControl::with_deadline_and_clock(remaining, &clock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn forwarding_retains_the_exact_short_parent_deadline() {
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(from_parent(deadline).deadline(), deadline);
    }

    #[test]
    fn forwarding_an_expired_parent_cannot_restart_its_budget() {
        let deadline = Instant::now() - Duration::from_secs(1);
        let control = from_parent(deadline);
        assert_eq!(control.deadline(), deadline);
        assert!(control.is_expired());
    }

    #[test]
    fn a_long_parent_still_obeys_the_existing_foreground_ceiling() {
        let before = Instant::now();
        let control = from_parent(before + Duration::from_secs(600));
        let after = Instant::now();
        assert!(control.deadline() >= before + InvocationControl::DEADLINE);
        assert!(control.deadline() <= after + InvocationControl::DEADLINE);
    }
}
