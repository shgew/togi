use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use togi_config::Config;
use togi_detect::Kernel;
use togi_machine::{Backend, CoreInfo};
use togi_mprime::Mprime;
use togi_smu::Driver;
use togi_trial::{Backends, Options, Runner};
use togi_ycruncher::Ycruncher;

use crate::adapters::{PmSlot, RunnerHandle, SharedDriver};
use crate::error::Error;
use crate::host::{Privilege, ScopeProbe, SystemHost, SystemReadings};
use crate::preflight::Account;

pub(crate) fn backends(config: &Config) -> Result<Backends, Error> {
    let mut backends = Backends::new();
    if let Some(path) = config.backends().path(Backend::Mprime) {
        let mprime = Mprime::new(path).map_err(|source| Error::Backend {
            backend: Backend::Mprime,
            source,
        })?;
        backends = backends.with(Arc::new(mprime));
    }
    if let Some(path) = config.backends().path(Backend::Ycruncher) {
        let ycruncher = Ycruncher::new(path).map_err(|source| Error::Backend {
            backend: Backend::Ycruncher,
            source,
        })?;
        backends = backends.with(Arc::new(ycruncher));
    }
    Ok(backends)
}

/// What the host, the trial runners and the diagnosis share: one driver, the backends the
/// configuration names, the account they run as and the slot the PM table reader appears in.
pub(crate) struct Parts {
    pub(crate) driver: SharedDriver,
    pub(crate) topology: Vec<CoreInfo>,
    pub(crate) pm: PmSlot,
    pub(crate) backends: Backends,
    pub(crate) account: Account,
}

impl Parts {
    pub(crate) fn new(driver: Driver, config: &Config) -> Result<Self, Error> {
        let topology = driver.topology().map_err(Error::Topology)?;
        Ok(Self {
            driver: Arc::new(Mutex::new(driver)),
            topology,
            pm: PmSlot::default(),
            backends: backends(config)?,
            account: Account::resolve(config.backend_user()),
        })
    }

    pub(crate) fn runner(&self, dir: PathBuf) -> Result<RunnerHandle, Error> {
        Runner::new(Options {
            dir,
            backends: self.backends.clone(),
            topology: self.topology.clone(),
            identity: self.account.identity(),
            pm_table: Some(Arc::new(self.pm.clone())),
        })
        .map(RunnerHandle::new)
        .map_err(Error::TrialRunner)
    }

    pub(crate) fn kernel(&self) -> Result<Kernel, Error> {
        Kernel::new(&self.topology).map_err(Error::KernelLog)
    }

    pub(crate) fn host(
        self,
        root: &Path,
        privilege: Privilege,
        scope: impl ScopeProbe + 'static,
    ) -> SystemHost {
        SystemHost {
            root: root.to_owned(),
            privilege,
            driver: self.driver,
            pm: self.pm,
            backends: self.backends,
            account: self.account,
            scope: Box::new(scope),
            readings: SystemReadings,
        }
    }
}
