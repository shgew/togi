use tokio::time::Instant;
use togi_machine::Stopped;

use crate::error::{CleanupFailure, Containment};
use crate::id::ScopeName;
use crate::instance::LiveInstance;
use crate::os::Os;
use crate::timing::Timing;

/// One absolute deadline for the whole teardown; every phase takes a slice of what remains.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    deadline: Instant,
    timing: Timing,
}

impl Budget {
    pub(crate) fn start(timing: Timing) -> Self {
        Self {
            deadline: Instant::now() + timing.teardown,
            timing,
        }
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// End of the shared SIGTERM grace.
    pub(crate) fn grace_end(&self) -> Instant {
        self.deadline.min(Instant::now() + self.timing.stop_grace)
    }

    /// End of the one slot every scope kill shares.
    pub(crate) fn kill_end(&self) -> Instant {
        self.deadline.min(Instant::now() + self.timing.kill_slot)
    }

    /// End of the output drain.
    pub(crate) fn drain_end(&self) -> Instant {
        self.deadline.min(Instant::now() + self.timing.drain)
    }
}

/// Failures in the order met; the first is never replaced.
#[derive(Debug, Default)]
pub(crate) struct Failures(Option<Containment>);

impl Failures {
    pub(crate) fn record(&mut self, failure: CleanupFailure) {
        match &mut self.0 {
            Some(containment) => containment.push(failure),
            None => self.0 = Some(Containment::new(failure)),
        }
    }

    pub(crate) fn into_option(self) -> Option<Containment> {
        self.0
    }
}

/// The trial's launched instances plus every scope attempted, also those of a failed launch.
pub(crate) struct Live<O: Os> {
    pub(crate) instances: Vec<LiveInstance<O>>,
    pub(crate) scopes: Vec<ScopeName>,
}

pub(crate) struct Running<O: Os>(Live<O>);
pub(crate) struct Reaped(());
pub(crate) struct ReadersDone(());

/// The teardown typestate: each step consumes the last and may only be reached through it.
pub(crate) struct Teardown<S> {
    budget: Budget,
    failures: Failures,
    state: S,
}

/// What a finished teardown leaves: classified final output and the proof or the failures.
pub(crate) struct Ended {
    pub(crate) proof: Result<Stopped, Containment>,
}

impl<O: Os> Teardown<Running<O>> {
    pub(crate) fn begin(live: Live<O>, budget: Budget) -> Self {
        Self {
            budget,
            failures: Failures::default(),
            state: Running(live),
        }
    }

    /// SIGCONT then SIGTERM to each group with a shared grace, SIGKILL through every known
    /// scope in one slot, SIGKILL to each unreaped group, then reap every leader.
    pub(crate) async fn reap(self, os: &O) -> Teardown<Reaped> {
        let _ = (os, &self.budget, &self.failures, &self.state.0);
        unimplemented!()
    }
}

impl Teardown<Reaped> {
    /// Drains pipes and watched files until the drain slot ends; both readers must hit end of
    /// input, and watched final partials count only for instances whose writers are confirmed gone.
    pub(crate) async fn drain(self) -> Teardown<ReadersDone> {
        let _ = (&self.budget, &self.failures, &self.state.0);
        unimplemented!()
    }
}

impl Teardown<ReadersDone> {
    /// Classifies exit events and final partials; the only place `Stopped` is minted.
    pub(crate) fn finish(self) -> Ended {
        let _ = (&self.budget, &self.failures, &self.state.0);
        unimplemented!()
    }
}
