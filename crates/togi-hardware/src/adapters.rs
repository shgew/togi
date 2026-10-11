use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use rustix::time::{ClockId, clock_gettime};
use togi_machine::{
    CancellationToken, Clock, CoreId, Error, ObservedOffset, PmTable, PmTableReader, Smu,
    SmuValidated, Stopped, TargetOffset,
};
use togi_smu::{Driver, PmReader};

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
pub(crate) struct PmSlot(Arc<OnceLock<Arc<PmReader>>>);

impl PmSlot {
    pub(crate) fn publish(&self, reader: Arc<PmReader>) {
        let _ = self.0.set(reader);
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
