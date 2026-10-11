use std::time::Duration;

/// How long a launcher may take to enter its scope, whatever the stall grace.
pub(crate) const READINESS_LIMIT: Duration = Duration::from_secs(30);

/// The runner's clock constants. Production uses `Timing::default()`; tests shorten them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Timing {
    pub sample: Duration,
    pub stall_grace: Duration,
    pub stall_window: Duration,
    pub stop_grace: Duration,
    pub teardown: Duration,
    pub kill_slot: Duration,
    pub drain: Duration,
    pub writer_join: Duration,
    pub probe: Duration,
    /// Slack the synchronous side adds to what the runtime thread is allowed to take.
    pub margin: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            sample: Duration::from_secs(1),
            stall_grace: Duration::from_secs(30),
            stall_window: Duration::from_secs(10),
            stop_grace: Duration::from_secs(3),
            teardown: Duration::from_secs(15),
            kill_slot: Duration::from_secs(2),
            drain: Duration::from_secs(10),
            writer_join: Duration::from_secs(5),
            probe: Duration::from_secs(30),
            margin: Duration::from_secs(5),
        }
    }
}

impl Timing {
    /// How long one launcher gets to enter its scope before R6 stops it anyway.
    pub(crate) fn readiness(&self) -> Duration {
        self.stall_grace.min(READINESS_LIMIT)
    }

    /// The most the runtime thread may need to start `instances` instances: each scope wait of an
    /// R6 start, then a rollback teardown if the start fails. Timing begins after it.
    pub(crate) fn start_bound(&self, instances: u32) -> Duration {
        self.readiness() * instances + self.teardown + self.margin
    }

    /// From the start's completion to the supervisor's report: the duration, the teardown and the
    /// samples writer's join.
    pub(crate) fn wait_bound(&self, duration: Duration) -> Duration {
        duration + self.stop_bound()
    }

    /// From a stop request to the supervisor's report: the teardown and the writer join.
    pub(crate) fn stop_bound(&self) -> Duration {
        self.teardown + self.writer_join + self.margin
    }

    pub(crate) fn sweep_bound(&self) -> Duration {
        self.teardown + self.margin
    }

    pub(crate) fn probe_bound(&self) -> Duration {
        self.probe + self.margin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_synchronous_bound_covers_what_the_runtime_thread_may_take() {
        let timing = Timing::default();
        let secs = Duration::from_secs;
        assert_eq!(timing.readiness(), secs(30));
        assert_eq!(timing.start_bound(1), secs(30 + 15 + 5));
        assert_eq!(timing.start_bound(16), secs(16 * 30 + 15 + 5));
        assert_eq!(timing.stop_bound(), secs(15 + 5 + 5));
        assert_eq!(timing.wait_bound(secs(900)), secs(900 + 15 + 5 + 5));
        assert_eq!(timing.sweep_bound(), secs(15 + 5));
        assert_eq!(timing.probe_bound(), secs(30 + 5));
        assert!(timing.wait_bound(secs(1)) > timing.teardown + timing.writer_join);
    }

    #[test]
    fn a_short_stall_grace_shortens_the_readiness_wait() {
        let timing = Timing {
            stall_grace: Duration::from_millis(500),
            ..Timing::default()
        };
        assert_eq!(timing.readiness(), Duration::from_millis(500));
    }
}
