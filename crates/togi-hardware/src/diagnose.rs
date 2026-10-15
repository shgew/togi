use std::path::{Path, PathBuf};

use togi_config::Config;
use togi_machine::{BiosContext, Check};
use togi_smu::Driver;

use crate::assemble::Parts;
use crate::checks::{CheckName, Skip, Skipped, failed, passed};
use crate::error::Error;
use crate::host::{HostReadings, Privilege, SystemHost};
use crate::preflight::Outcome;

/// What `togi doctor` found. The checks made before a BIOS context read failed stay in `ran`.
#[derive(Debug)]
pub struct Diagnosis {
    pub ran: Vec<Check>,
    pub skipped: Vec<Skipped>,
    pub bios_context_error: Option<togi_smu::Error>,
}

pub(crate) fn compare_bios_context(recorded: &BiosContext, current: &BiosContext) -> Check {
    match recorded.first_difference(current) {
        None => passed(CheckName::BiosContext, "matches the session"),
        Some(mismatch) => failed(
            CheckName::BiosContext,
            format!("{mismatch}; run archives this session and starts a new one"),
        ),
    }
}

/// What a diagnosis asks of the machine.
pub(crate) trait Survey {
    fn steps(&self, privileged: bool) -> Vec<Outcome>;
    fn watchdog(&self) -> Check;
    fn bios_context(&self) -> Result<BiosContext, togi_smu::Error>;
}

impl<R: HostReadings> Survey for SystemHost<R> {
    fn steps(&self, privileged: bool) -> Vec<Outcome> {
        self.run_steps(privileged)
    }

    fn watchdog(&self) -> Check {
        self.watchdog_check()
    }

    fn bios_context(&self) -> Result<BiosContext, togi_smu::Error> {
        self.current_bios_context()
    }
}

pub(crate) fn diagnose_with(
    survey: &dyn Survey,
    recorded: Option<&BiosContext>,
    privileged: bool,
) -> Diagnosis {
    let mut ran = Vec::new();
    let mut skipped = Vec::new();
    for outcome in survey.steps(privileged) {
        match outcome {
            Outcome::Ran(check) => ran.push(check),
            Outcome::Skipped(skip) => skipped.push(skip),
        }
    }
    let all_passed = ran.iter().all(|check| check.ok);
    ran.push(survey.watchdog());
    let mut bios_context_error = None;
    if let Some(recorded) = recorded {
        let refusal = if !privileged {
            Some(Skip::NeedsRoot)
        } else if !all_passed {
            Some(Skip::NeedsOtherChecks)
        } else {
            None
        };
        match refusal {
            Some(reason) => skipped.push(Skipped {
                name: CheckName::BiosContext,
                reason,
            }),
            None => match survey.bios_context() {
                Ok(current) => ran.push(compare_bios_context(recorded, &current)),
                Err(error) => bios_context_error = Some(error),
            },
        }
    }
    Diagnosis {
        ran,
        skipped,
        bios_context_error,
    }
}

pub(crate) fn diagnose_at(
    root: &Path,
    config: &Config,
    recorded: Option<&BiosContext>,
    request_privileged: bool,
    privilege: Privilege,
) -> Result<Diagnosis, Error> {
    let (host, privileged) = diagnosis_host(root, config, request_privileged, privilege)?;
    Ok(diagnose_with(&host, recorded, privileged))
}

pub(crate) fn diagnosis_host(
    root: &Path,
    config: &Config,
    request_privileged: bool,
    privilege: Privilege,
) -> Result<(SystemHost, bool), Error> {
    diagnosis_host_with(
        request_privileged,
        privilege,
        || Driver::open(root),
        || Driver::identity_only(root),
        |driver| {
            let parts = Parts::new(driver, config)?;
            let scope = parts.runner(PathBuf::new())?;
            Ok(parts.host(root, privilege, scope))
        },
    )
}

pub(crate) fn diagnosis_host_with<H>(
    request_privileged: bool,
    privilege: Privilege,
    open: impl FnOnce() -> Result<Driver, togi_smu::Error>,
    identity_only: impl FnOnce() -> Result<Driver, togi_smu::Error>,
    build: impl FnOnce(Driver) -> Result<H, Error>,
) -> Result<(H, bool), Error> {
    let privileged = request_privileged && privilege.is_root();
    let driver = if privileged { open() } else { identity_only() }.map_err(Error::OpenSmu)?;
    Ok((build(driver)?, privileged))
}

#[cfg(test)]
mod tests;
