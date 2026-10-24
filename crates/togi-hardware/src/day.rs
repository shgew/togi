//! Read-only hardware access for daily observation, with no offset-writing capability.

use std::marker::PhantomData;
use std::path::Path;

use togi_hostlock::HostLock;
use togi_machine::{
    BiosContext, BootId, CoreInfo, EndEvidence, Error, Kernel as _, ObservedOffset, PerCore,
    ResetReason, Smu as _, WatchdogState,
};
use togi_smu::Driver;

/// The observations a day boot needs. Implementations never apply offsets or start workloads.
pub trait Observer {
    /// Firmware context, read after validating the SMU identity.
    fn bios_context(&mut self) -> Result<BiosContext, Error>;
    /// Reads every online physical core through the normal SMU read path.
    fn offsets(&mut self) -> Result<PerCore<ObservedOffset>, Error>;
    /// Reset evidence from the proven immediate successor of `boot`.
    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error>;
    /// What the kernel logs of the crashed `boot` and of its proven immediate successor say about how it ended.
    /// Infallible: whatever cannot be read is unknown in the result, never an absence of errors.
    fn end_evidence(&mut self, boot: &BootId) -> EndEvidence;
    /// Whether PID 1's hardware watchdog is armed right now, for the day service and the gate that applies
    /// offsets only while a hang would reset the machine.
    fn watchdog(&mut self) -> WatchdogState;
}

struct ReadOnly<'lock> {
    driver: Driver,
    cores: Vec<CoreInfo>,
    kernel: togi_detect::Kernel,
    lock: PhantomData<&'lock HostLock>,
}

/// Opens only the SMU reader and kernel evidence reader, borrowing hardware exclusion throughout.
///
/// # Errors
/// Returns a platform, identity, topology or kernel-reader setup error before observation starts.
pub fn observe(_lock: &HostLock) -> Result<Box<dyn Observer + '_>, Error> {
    crate::check_platform().map_err(Error::from)?;
    let mut driver = Driver::system().map_err(Error::from)?;
    let _validated = driver.validate().map_err(Error::from)?;
    let cores = driver.topology().map_err(Error::from)?;
    let kernel = togi_detect::Kernel::new(&cores)
        .map_err(|source| Error::from(crate::Error::KernelLog(source)))?;
    Ok(Box::new(ReadOnly {
        driver,
        cores,
        kernel,
        lock: PhantomData,
    }))
}

impl Observer for ReadOnly<'_> {
    fn bios_context(&mut self) -> Result<BiosContext, Error> {
        self.driver.bios_context().map_err(Error::from)
    }

    fn offsets(&mut self) -> Result<PerCore<ObservedOffset>, Error> {
        let mut offsets = PerCore::default();
        for info in &self.cores {
            offsets.set(info.core, self.driver.offset(info.core)?);
        }
        Ok(offsets)
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        self.kernel.reset_reason_after(boot)
    }

    fn end_evidence(&mut self, boot: &BootId) -> EndEvidence {
        self.kernel.end_evidence(boot)
    }

    fn watchdog(&mut self) -> WatchdogState {
        crate::watchdog::arming(Path::new("/"))
    }
}
