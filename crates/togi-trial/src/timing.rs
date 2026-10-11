use std::time::Duration;

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
        }
    }
}
