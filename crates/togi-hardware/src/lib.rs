//! The real machine: the SMU driver, the host's preflight and watchdog, trials, the kernel log, GRUB's
//! environment and the `togi doctor` diagnosis.

mod adapters;
mod assemble;
mod checks;
mod diagnose;
mod error;
mod files;
mod grub;
mod host;
mod preflight;
mod ranking;
mod reboot;
mod watchdog;

use std::path::Path;

use togi_config::Config;
use togi_hostlock::HostLock;
use togi_machine::{BiosContext, Machine};
use togi_smu::Driver;

pub use checks::{CheckName, Skip, Skipped};
pub use diagnose::Diagnosis;
pub use error::Error;
pub use grub::{ClearError, CommandError, Grub, GrubError, Verb};
pub use reboot::{RebootError, reboot};

use adapters::{SharedSmu, SystemClock};
use assemble::Parts;
use host::Privilege;

/// Whether this platform can run on real hardware.
pub fn check_platform() -> Result<(), Error> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        Err(Error::Unsupported)
    }
}

const SYSTEM_ROOT: &str = "/";

/// Assembles the machine under the held host lock. Reads the topology and nothing else from the
/// hardware: no mailbox, SMN or PM table access happens before `Host::validate_smu`.
///
/// Hardware access cannot outlive the caller-owned lock, even when a seam is taken out of the
/// machine:
///
/// ```compile_fail,E0505
/// use std::path::Path;
/// use togi_config::Config;
/// use togi_hostlock::HostLock;
/// use togi_machine::CoreId;
///
/// fn release_while_accessible(config: &Config, state: &Path, lock: HostLock) {
///     let machine = togi_hardware::open(config, state, &lock).unwrap();
///     let mut smu = machine.smu;
///     drop(lock);
///     let _ = smu.offset(CoreId::new(0).unwrap());
/// }
/// ```
pub fn open<'lock>(
    config: &Config,
    state_dir: &Path,
    _lock: &'lock HostLock,
) -> Result<Machine<'lock>, Error> {
    check_platform()?;
    let root = Path::new(SYSTEM_ROOT);
    let parts = Parts::new(Driver::open(root).map_err(Error::OpenSmu)?, config)?;
    let trials = parts.runner(state_dir.join("trials"))?;
    let kernel = parts.kernel()?;
    let smu = SharedSmu(std::sync::Arc::clone(&parts.driver));
    let host = parts.host(root, Privilege::current(), trials.clone());
    Ok(Machine {
        clock: Box::new(SystemClock),
        smu: Box::new(smu),
        host: Box::new(host),
        trials: Box::new(trials),
        kernel: Box::new(kernel),
    })
}

/// Makes `run`'s preflight checks without writing anything. Privileged checks require both the
/// held host lock and root. Otherwise the driver only checks identity and touches no mailbox or PM table.
///
/// A boolean cannot authorize privileged checks:
///
/// ```compile_fail,E0308
/// use togi_config::Config;
///
/// let _ = togi_hardware::diagnose(&Config::default(), None, true);
/// ```
pub fn diagnose(
    config: &Config,
    recorded: Option<&BiosContext>,
    lock: Option<&HostLock>,
) -> Result<Diagnosis, Error> {
    check_platform()?;
    diagnose::diagnose_at(
        Path::new(SYSTEM_ROOT),
        config,
        recorded,
        lock.is_some(),
        Privilege::current(),
    )
}

#[cfg(test)]
mod tests;
