use std::time::Duration;

/// The stall rule: over a window of unsuspended time, CPU time must advance by at least half of
/// the allowed CPUs times the window, after a startup grace.
#[derive(Debug)]
pub(crate) struct Stall {
    grace: Duration,
    window: Duration,
    cpus: u32,
    history: Vec<(Duration, Duration)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stalled {
    pub(crate) cpu_seconds: f64,
    pub(crate) running_seconds: f64,
}

impl Stall {
    pub(crate) fn new(grace: Duration, window: Duration, cpus: u32) -> Self {
        Self {
            grace,
            window,
            cpus,
            history: Vec::new(),
        }
    }

    /// `active` is unsuspended time since timing began, `cpu` the cumulative CPU time.
    pub(crate) fn observe(&mut self, active: Duration, cpu: Duration) -> Option<Stalled> {
        let _ = (active, cpu, self.grace, self.window, self.cpus, &self.history);
        unimplemented!()
    }
}
