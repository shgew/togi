use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustix::process::geteuid;
use togi_machine::{
    BackendCheck, BiosContext, BootId, Check, CoreInfo, CoreRank, Error as MachineError, Host,
    Partial, SmuValidated, error_chain,
};
use togi_smu::Driver;
use togi_trial::{Backends, Runner};

use crate::adapters::{PmSlot, SharedDriver, lock};
use crate::checks::{CheckName, failed, outcome, passed};
use crate::error::Error;
use crate::preflight::{self, Account, Outcome, Step};
use crate::{ranking, watchdog};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Privilege {
    Root,
    User(u32),
}

impl Privilege {
    pub(crate) fn current() -> Self {
        let uid = geteuid();
        if uid.is_root() {
            Self::Root
        } else {
            Self::User(uid.as_raw())
        }
    }

    pub(crate) const fn is_root(self) -> bool {
        matches!(self, Self::Root)
    }

    fn require(self, operation: &'static str) -> Result<(), Error> {
        match self {
            Self::Root => Ok(()),
            Self::User(uid) => Err(Error::NeedsRoot { operation, uid }),
        }
    }
}

/// Creates a systemd scope pinned to CPU 0, as preflight proves it can.
pub(crate) trait ScopeProbe {
    fn probe(&self) -> Result<String, togi_trial::Error>;
}

impl ScopeProbe for Runner {
    fn probe(&self) -> Result<String, togi_trial::Error> {
        self.probe_scope()
    }
}

pub(crate) trait HostReadings {
    fn watchdog(&self, root: &Path) -> Check;
    fn ranking(
        &self,
        root: &Path,
        cores: &[CoreInfo],
    ) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>>;
}

pub(crate) struct SystemReadings;

impl HostReadings for SystemReadings {
    fn watchdog(&self, root: &Path) -> Check {
        watchdog::probe(root)
    }

    fn ranking(
        &self,
        root: &Path,
        cores: &[CoreInfo],
    ) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
        ranking::read(root, cores)
    }
}

pub(crate) struct SystemHost<R = SystemReadings> {
    pub(crate) root: PathBuf,
    pub(crate) privilege: Privilege,
    pub(crate) driver: SharedDriver,
    pub(crate) pm: PmSlot,
    pub(crate) backends: Backends,
    pub(crate) account: Account,
    pub(crate) scope: Box<dyn ScopeProbe>,
    pub(crate) readings: R,
}

impl<R: HostReadings> SystemHost<R> {
    fn driver_check(&self, name: CheckName, check: impl FnOnce(&mut Driver) -> Check) -> Check {
        let mut driver = lock(&self.driver);
        match driver.validate() {
            Ok(_) => check(&mut driver),
            Err(error) => failed(name, error_chain(&error)),
        }
    }

    fn pm_table_check(&self) -> Check {
        let reader = {
            let mut driver = lock(&self.driver);
            driver.validate().and_then(|_| driver.pm_reader())
        };
        self.check_pm_reader(reader)
    }

    pub(crate) fn check_pm_reader(
        &self,
        reader: Result<Arc<togi_smu::PmReader>, togi_smu::Error>,
    ) -> Check {
        match reader {
            Ok(reader) => {
                self.pm.publish(Arc::clone(&reader));
                reader.check()
            }
            Err(error) => passed(
                CheckName::PmTable,
                format!("pm_table unavailable: {}", error_chain(&error)),
            ),
        }
    }

    fn systemd_run_check(&self) -> Check {
        match &self.account {
            Account::Resolved { .. } => outcome(CheckName::SystemdRun, self.scope.probe()),
            Account::Refused(error) => failed(
                CheckName::SystemdRun,
                format!(
                    "systemd-run needs a usable backend_user: {}",
                    error_chain(error)
                ),
            ),
        }
    }

    fn step(&self, step: Step) -> Check {
        match step {
            Step::Cpu => lock(&self.driver).check_cpu(),
            Step::RyzenSmu => lock(&self.driver).check_driver(),
            Step::Readback => self.driver_check(CheckName::Readback, Driver::check_readback),
            Step::PmTable => self.pm_table_check(),
            Step::SlotMapping => {
                self.driver_check(CheckName::SlotMapping, Driver::check_slot_mapping)
            }
            Step::Backends => preflight::check_backends(&self.backends),
            Step::BackendUser => self.account.check(),
            Step::SystemdRun => self.systemd_run_check(),
        }
    }

    pub(crate) fn run_steps(&self, steps: &[Step], privileged: bool) -> Vec<Outcome> {
        preflight::run(steps, privileged, |step| self.step(step))
    }

    pub(crate) fn watchdog_check(&self) -> Check {
        self.readings.watchdog(&self.root)
    }

    pub(crate) fn current_bios_context(&self) -> Result<BiosContext, togi_smu::Error> {
        lock(&self.driver).bios_context()
    }

    fn root_check(&self) -> Check {
        match self.privilege {
            Privilege::Root => passed(CheckName::Root, "uid 0"),
            Privilege::User(uid) => failed(
                CheckName::Root,
                format!("running as uid {uid}; run needs root"),
            ),
        }
    }
}

impl<R: HostReadings> Host for SystemHost<R> {
    fn boot_id(&mut self) -> Result<BootId, MachineError> {
        Ok(togi_detect::boot_id()?)
    }

    fn topology(&mut self) -> Result<Vec<CoreInfo>, MachineError> {
        Ok(lock(&self.driver).topology()?)
    }

    fn validate_smu(&mut self) -> Result<SmuValidated, MachineError> {
        self.privilege.require("validate the SMU")?;
        let mut driver = lock(&self.driver);
        let proof = driver.validate()?;
        if let Ok(reader) = driver.pm_reader() {
            self.pm.publish(reader);
        }
        Ok(proof)
    }

    fn bios_context(&mut self) -> Result<BiosContext, MachineError> {
        Ok(self.current_bios_context()?)
    }

    fn ranking(&mut self) -> Result<Vec<CoreRank>, Partial<Vec<CoreRank>>> {
        let cores = lock(&self.driver).topology().map_err(|error| Partial {
            value: Vec::new(),
            error: error.into(),
        })?;
        self.readings.ranking(&self.root, &cores)
    }

    fn preflight(&mut self) -> Vec<Check> {
        let outcomes = self.run_steps(&preflight::SAFETY_STEPS, self.privilege.is_root());
        std::iter::once(self.root_check())
            .chain(outcomes.into_iter().map(Outcome::into_check))
            .collect()
    }

    fn backends(&mut self) -> Vec<BackendCheck> {
        preflight::backend_checks(&self.backends)
    }

    fn watchdog(&mut self) -> Check {
        self.watchdog_check()
    }
}
