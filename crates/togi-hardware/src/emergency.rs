//! Abnormal-exit recovery: verify the stale-scope sweep, then zero and read back every core.

use std::path::Path;

use togi_hostlock::{DEFAULT_PATH, RefusedEntry};
use togi_machine::emergency::{Zeroed, zero_and_read_back};
use togi_machine::{
    CancellationToken, CoreInfo, CoreSet, Error as MachineError, Machine, Smu, SmuValidated, Trials,
};
use togi_smu::Driver;
use togi_trial::{Backends, Options, Runner};

use crate::{Error, check_platform, host::Privilege};

/// Why the emergency could not start writing offsets.
#[derive(Debug, thiserror::Error)]
pub enum Failure {
    #[error(transparent)]
    Open(#[from] Error),
    #[error("the refusal was not for the installed host lock")]
    UnrelatedRefusal,
    #[error("read the core topology")]
    Topology(#[source] MachineError),
    #[error("validate the SMU interface")]
    Validation(#[source] MachineError),
    #[error("stop the stale trial scopes")]
    Sweep(#[source] MachineError),
}

#[derive(Debug)]
#[must_use = "every core must read back zero before recovery can report success"]
pub struct Recovery {
    pub cores: usize,
    pub zeroed: Zeroed,
}

/// Recovers under the lock borrowed by a real `Machine`. `swept` reports confirmed containment
/// before the write starts; it cannot veto recovery.
pub fn stop_locked(
    machine: &mut Machine<'_>,
    swept: impl FnOnce(&str),
) -> Result<Recovery, Failure> {
    let topology = machine.host.topology().map_err(Failure::Topology)?;
    let cores = core_set(&topology)?;
    let validated = machine.host.validate_smu().map_err(Failure::Validation)?;
    sweep_and_zero(
        &mut *machine.smu,
        &mut *machine.trials,
        cores,
        validated,
        swept,
    )
}

/// The only hardware capability available after refusal of the installed host lock's final entry.
/// It cannot expose a driver, change a nonzero offset, or start a workload.
///
/// A refusal cannot authorize the regular hardware opener:
///
/// ```compile_fail,E0308
/// use std::path::Path;
/// use togi_config::Config;
/// use togi_hostlock::RefusedEntry;
///
/// fn not_a_lock(refused: &RefusedEntry) {
///     let _ = togi_hardware::open(&Config::default(), Path::new("/var/lib/togi"), refused);
/// }
/// ```
#[must_use = "the refused lock permits only one attempt at zeroing and reading back offsets"]
pub struct UnexcludedStop<'a> {
    _refused: &'a RefusedEntry,
    driver: Driver,
    trials: Runner,
    cores: CoreSet,
}

impl<'a> UnexcludedStop<'a> {
    /// Opens the minimal hardware needed for zero-only recovery. It opens neither a journal nor a
    /// PM table reader, and configures no workload and no account that could run one.
    pub fn open(refused: &'a RefusedEntry, state_dir: &Path) -> Result<Self, Failure> {
        check_platform()?;
        Privilege::current().require("open the emergency SMU")?;
        if refused.path() != Path::new(DEFAULT_PATH) {
            return Err(Failure::UnrelatedRefusal);
        }
        let driver = Driver::system().map_err(Error::OpenSmu)?;
        let topology = driver.topology().map_err(Error::Topology)?;
        let cores = core_set(&topology)?;
        let trials = Runner::new(Options {
            dir: state_dir.join("trials"),
            backends: Backends::new(),
            topology,
            identity: None,
            pm_table: None,
        })
        .map_err(Error::TrialRunner)?;
        Ok(Self {
            _refused: refused,
            driver,
            trials,
            cores,
        })
    }

    /// Validates the SMU and verifies containment before the zero-only write and every-core readback.
    pub fn recover(mut self, swept: impl FnOnce(&str)) -> Result<Recovery, Failure> {
        let validated = self
            .driver
            .validate()
            .map_err(MachineError::from)
            .map_err(Failure::Validation)?;
        sweep_and_zero(
            &mut self.driver,
            &mut self.trials,
            self.cores,
            validated,
            swept,
        )
    }
}

fn core_set(topology: &[CoreInfo]) -> Result<CoreSet, Failure> {
    if topology.is_empty() {
        return Err(Failure::Topology(MachineError::Other(
            "the topology lists no core to zero".into(),
        )));
    }
    Ok(topology.iter().map(|info| info.core).collect())
}

#[expect(
    clippy::disallowed_methods,
    reason = "emergency-stop's zero-only write, after a validated SMU and a confirmed sweep"
)]
fn sweep_and_zero(
    smu: &mut dyn Smu,
    trials: &mut dyn Trials,
    cores: CoreSet,
    validated: SmuValidated,
    report_sweep: impl FnOnce(&str),
) -> Result<Recovery, Failure> {
    let swept = trials
        .sweep(&CancellationToken::new())
        .map_err(Failure::Sweep)?;
    report_sweep(&swept.detail);
    let zeroed = zero_and_read_back(smu, validated, &swept.stopped, cores);
    Ok(Recovery {
        cores: cores.len(),
        zeroed,
    })
}
