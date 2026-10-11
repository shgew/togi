use std::time::Duration;

/// The stall rule: over a window of unsuspended time, CPU time must advance by at least half of
/// the allowed CPUs times the window, after a startup grace. Equality is healthy.
#[derive(Debug)]
pub(crate) struct Stall {
    grace: Duration,
    window: Duration,
    cpus: u32,
    history: Vec<(Duration, Duration)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stalled {
    pub(crate) cpu: Duration,
    pub(crate) running: Duration,
}

impl Stalled {
    pub(crate) fn detail(&self) -> String {
        format!(
            "{:.1}s cpu in {:.1}s running",
            self.cpu.as_secs_f64(),
            self.running.as_secs_f64()
        )
    }
}

impl Stall {
    pub(crate) const fn new(grace: Duration, window: Duration, cpus: u32) -> Self {
        Self {
            grace,
            window,
            cpus,
            history: Vec::new(),
        }
    }

    /// `active` is unsuspended time since timing began, `cpu` the cumulative CPU time. Samples
    /// before the grace never form a window and are not kept; older ones are dropped as the
    /// window advances.
    pub(crate) fn observe(&mut self, active: Duration, cpu: Duration) -> Option<Stalled> {
        if active < self.grace {
            return None;
        }
        self.history.push((active, cpu));
        let window = self.window;
        let latest = self
            .history
            .iter()
            .rposition(|&(earlier, _)| active.saturating_sub(earlier) >= window)?;
        let (earlier_active, earlier_cpu) = self.history[latest];
        self.history.drain(..latest);
        let running = active.saturating_sub(earlier_active);
        let used = cpu.saturating_sub(earlier_cpu);
        (!running.is_zero() && used < running.mul_f64(0.5 * f64::from(self.cpus)))
            .then_some(Stalled { cpu: used, running })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn strictly_less_than_half_a_cpu_per_cpu_stalls_and_equality_is_healthy() {
        let mut healthy = Stall::new(secs(30), secs(10), 2);
        assert_eq!(healthy.observe(secs(30), secs(100)), None);
        assert_eq!(healthy.observe(secs(40), secs(110)), None);
        let mut stalled = Stall::new(secs(30), secs(10), 2);
        stalled.observe(secs(30), secs(100));
        let result = stalled.observe(secs(40), Duration::from_millis(109_999));
        assert_eq!(
            result,
            Some(Stalled {
                cpu: Duration::from_millis(9_999),
                running: secs(10)
            })
        );
    }

    #[test]
    fn nothing_is_judged_before_the_grace_or_a_full_window() {
        let mut stall = Stall::new(secs(30), secs(10), 1);
        assert_eq!(stall.observe(secs(5), secs(0)), None);
        assert_eq!(stall.observe(secs(29), secs(0)), None);
        assert_eq!(stall.observe(secs(30), secs(0)), None);
        assert_eq!(stall.observe(secs(39), secs(0)), None);
        assert!(stall.observe(secs(40), secs(0)).is_some());
    }

    #[test]
    fn detail_reads_like_the_report() {
        let stalled = Stalled {
            cpu: Duration::from_millis(2_500),
            running: secs(10),
        };
        assert_eq!(stalled.detail(), "2.5s cpu in 10.0s running");
    }

    #[test]
    fn history_does_not_grow_without_bound() {
        let mut stall = Stall::new(secs(0), secs(10), 1);
        for second in 0..1_000 {
            stall.observe(secs(second), secs(second));
        }
        assert!(stall.history.len() <= 12);
    }

    #[test]
    fn a_decreasing_counter_counts_as_no_progress() {
        let mut stall = Stall::new(secs(0), secs(10), 1);
        stall.observe(secs(0), secs(50));
        assert!(stall.observe(secs(10), secs(10)).is_some());
    }
}
