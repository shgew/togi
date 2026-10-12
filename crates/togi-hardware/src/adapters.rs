use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use rustix::time::{ClockId, clock_gettime};
use togi_machine::{
    CancellationToken, Clock, CoreId, Error, ObservedOffset, Partial, PmTable, PmTableReader,
    Running, Smu, SmuValidated, Stopped, Swept, TargetOffset, TrialConditions, TrialSpec, Trials,
};
use togi_smu::Driver;
use togi_trial::Runner;

use crate::host::ScopeProbe;

pub(crate) type SharedDriver = Arc<Mutex<Driver>>;

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The driver's `Smu` face; the host holds the same driver for validation and checks.
pub(crate) struct SharedSmu(pub(crate) SharedDriver);

impl Smu for SharedSmu {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, Error> {
        lock(&self.0).offset(core)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "delegates the session's write transaction to the one driver"
    )]
    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), Error> {
        lock(&self.0).set_offset(core, offset)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "delegates the session's write transaction to the one driver"
    )]
    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error> {
        lock(&self.0).set_all_offsets(offset)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "delegates the session's emergency zeroing, with its proofs, to the one driver"
    )]
    fn emergency_zero(&mut self, validated: SmuValidated, stopped: &Stopped) -> Result<(), Error> {
        lock(&self.0).emergency_zero(validated, stopped)
    }
}

/// The PM table reader, which exists only once the driver validated: trials ask it from the start
/// and get no table until the host publishes the reader.
#[derive(Clone, Default)]
pub(crate) struct PmSlot(Arc<OnceLock<Arc<dyn PmTableReader + Send + Sync>>>);

impl PmSlot {
    pub(crate) fn publish(&self, reader: Arc<impl PmTableReader + Send + Sync + 'static>) {
        let _ = self.0.set(reader);
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn is_published(&self) -> bool {
        self.0.get().is_some()
    }
}

impl PmTableReader for PmSlot {
    fn pm_table(&self) -> Option<PmTable> {
        self.0.get()?.pm_table()
    }
}

pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn monotonic(&self) -> Duration {
        let now = clock_gettime(ClockId::Monotonic);
        Duration::new(
            u64::try_from(now.tv_sec).unwrap_or_default(),
            u32::try_from(now.tv_nsec).unwrap_or_default(),
        )
    }

    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        if cancel.wait_timeout(duration) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// The one trial runner, shared by the machine's `Trials` and the preflight's scope probe, so a
/// process has a single runtime thread.
#[derive(Clone)]
pub(crate) struct RunnerHandle(Arc<Mutex<Runner>>);

impl RunnerHandle {
    pub(crate) fn new(runner: Runner) -> Self {
        Self(Arc::new(Mutex::new(runner)))
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn is_unlocked(&self) -> bool {
        self.0.try_lock().is_ok()
    }
}

impl ScopeProbe for RunnerHandle {
    fn probe(&self) -> Result<String, togi_trial::Error> {
        lock(&self.0).probe_scope()
    }
}

impl Trials for RunnerHandle {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        lock(&self.0).sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        lock(&self.0).start(cancel, spec, ready)
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        lock(&self.0).ended(id)
    }

    /// A guard cannot outlive the call, so the trial's samples are read before it returns.
    fn samples<'a>(&'a self, id: &'a str) -> Box<dyn Iterator<Item = TrialConditions> + 'a> {
        let samples: Vec<_> = lock(&self.0).samples(id).collect();
        Box::new(samples.into_iter())
    }
}
