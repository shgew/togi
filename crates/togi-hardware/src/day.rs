//! Hardware and kernel evidence for the applying day service.

use std::marker::PhantomData;
use std::path::Path;

use togi_hostlock::HostLock;
use togi_machine::{
    BiosContext, BootId, CancellationToken, CoreInfo, CpuTime, EndEvidence, Error, Kernel as _,
    KernelCursor, KernelRead, ObservedOffset, Partial, PerCore, ResetReason, Smu, SmuValidated,
    Stopped, TargetOffset, Trials as _, WatchdogState,
};
use togi_smu::Driver;

/// The hardware and observations a day boot needs; it never starts workloads.
pub trait Observer: Smu {
    /// Recheck the driver's write authority before constrained emergency cleanup.
    fn validated(&mut self) -> Result<SmuValidated, Error>;
    /// Remove stale togi workloads and confirm none remain before a day write.
    fn stop_workloads(&mut self) -> Result<Stopped, Error>;
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
    /// The faults the running `boot`'s kernel log holds after `after`, or in the whole boot when `after` is `None`.
    /// An unresumable cursor is `Error::CursorMissing`; the partial value keeps what was read before a failure.
    fn live_faults(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<KernelRead>>;
    /// Every logical CPU's cumulative scheduler time since boot.
    fn cpu_times(&mut self) -> Result<Vec<CpuTime>, Error>;
}

struct DayHardware<'lock> {
    driver: Driver,
    cores: Vec<CoreInfo>,
    kernel: togi_detect::Kernel,
    lock: PhantomData<&'lock HostLock>,
}

/// Opens day hardware and kernel evidence, borrowing hardware exclusion throughout.
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
    Ok(Box::new(DayHardware {
        driver,
        cores,
        kernel,
        lock: PhantomData,
    }))
}

impl Observer for DayHardware<'_> {
    fn validated(&mut self) -> Result<SmuValidated, Error> {
        self.driver.validate().map_err(Error::from)
    }
    fn stop_workloads(&mut self) -> Result<Stopped, Error> {
        // With no backend identity or backends this runner cannot launch anything. Its only
        // purpose is the same stale-scope sweep that supplies the night session's stop proof.
        let mut runner = togi_trial::Runner::new(togi_trial::Options {
            dir: "/var/lib/togi".into(),
            backends: togi_trial::Backends::new(),
            topology: self.cores.clone(),
            identity: None,
            pm_table: None,
        })
        .map_err(|source| Error::from(crate::Error::TrialRunner(source)))?;
        Ok(runner.sweep(&CancellationToken::new())?.stopped)
    }
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

    fn live_faults(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<KernelRead>> {
        self.kernel.read_mces(boot, after)
    }

    fn cpu_times(&mut self) -> Result<Vec<CpuTime>, Error> {
        Ok(self.kernel.read_cpu_times()?)
    }
}

impl Smu for DayHardware<'_> {
    fn offset(&mut self, core: togi_machine::CoreId) -> Result<ObservedOffset, Error> {
        self.driver.offset(core)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "delegates the session receipt-authorized transaction to the driver"
    )]
    fn set_offset(
        &mut self,
        core: togi_machine::CoreId,
        offset: TargetOffset,
    ) -> Result<(), Error> {
        self.driver.set_offset(core, offset)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "delegates the session receipt-authorized transaction to the driver"
    )]
    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), Error> {
        self.driver.set_all_offsets(offset)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "delegates the zero-only cleanup with its validated and stopped proofs"
    )]
    fn emergency_zero(&mut self, validated: SmuValidated, stopped: &Stopped) -> Result<(), Error> {
        self.driver.emergency_zero(validated, stopped)
    }
}
