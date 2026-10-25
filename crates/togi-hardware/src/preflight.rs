use togi_machine::{Backend, BackendCheck, Check, error_chain};
use togi_trial::{BackendIdentity, Backends, IdentityError};

use crate::checks::{CheckName, Skip, Skipped, failed, passed};

/// A check of the preflight, in the order `run` makes them. `root` and `watchdog` are not steps:
/// the first is the caller's own, the second does not gate the others.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Step {
    Cpu,
    RyzenSmu,
    Readback,
    PmTable,
    SlotMapping,
    Backends,
    BackendUser,
    SystemdRun,
}

pub(crate) const STEPS: [Step; 8] = [
    Step::Cpu,
    Step::RyzenSmu,
    Step::Readback,
    Step::PmTable,
    Step::SlotMapping,
    Step::Backends,
    Step::BackendUser,
    Step::SystemdRun,
];

/// The mandatory safety steps `run` makes: every step but backend availability, which `Host::backends` reports
/// per backend.
pub(crate) const SAFETY_STEPS: [Step; 7] = [
    Step::Cpu,
    Step::RyzenSmu,
    Step::Readback,
    Step::PmTable,
    Step::SlotMapping,
    Step::BackendUser,
    Step::SystemdRun,
];

impl Step {
    pub(crate) const fn name(self) -> CheckName {
        match self {
            Self::Cpu => CheckName::Cpu,
            Self::RyzenSmu => CheckName::RyzenSmu,
            Self::Readback => CheckName::Readback,
            Self::PmTable => CheckName::PmTable,
            Self::SlotMapping => CheckName::SlotMapping,
            Self::Backends => CheckName::Backends,
            Self::BackendUser => CheckName::BackendUser,
            Self::SystemdRun => CheckName::SystemdRun,
        }
    }

    const fn establishes_identity(self) -> bool {
        matches!(self, Self::Cpu | Self::RyzenSmu)
    }

    /// Reads the SMU mailbox or the PM table, or creates a systemd scope.
    const fn needs_root(self) -> bool {
        matches!(
            self,
            Self::Readback | Self::PmTable | Self::SlotMapping | Self::SystemdRun
        )
    }

    /// Runs only on a machine whose CPU and driver were identified.
    const fn needs_identity(self) -> bool {
        !self.establishes_identity()
    }
}

pub(crate) enum Outcome {
    Ran(Check),
    Skipped(Skipped),
}

impl Outcome {
    pub(crate) fn into_check(self) -> Check {
        match self {
            Self::Ran(check) => check,
            Self::Skipped(skipped) => skipped.into_check(),
        }
    }
}

/// Whether `step` is skipped, given the privilege of the caller and whether the identity checks
/// before it passed. Root is asked first, so an unprivileged caller is told what it lacks.
pub(crate) const fn skip(step: Step, privileged: bool, identified: bool) -> Option<Skip> {
    if step.needs_root() && !privileged {
        Some(Skip::NeedsRoot)
    } else if step.needs_identity() && !identified {
        Some(Skip::NeedsIdentity)
    } else {
        None
    }
}

/// Runs `steps` in order. `identified` falls once an identity check fails.
pub(crate) fn run(
    steps: &[Step],
    privileged: bool,
    mut make: impl FnMut(Step) -> Check,
) -> Vec<Outcome> {
    let mut identified = true;
    let mut outcomes = Vec::with_capacity(steps.len());
    for &step in steps {
        if let Some(reason) = skip(step, privileged, identified) {
            outcomes.push(Outcome::Skipped(Skipped {
                name: step.name(),
                reason,
            }));
            continue;
        }
        let check = make(step);
        if step.establishes_identity() && !check.ok {
            identified = false;
        }
        outcomes.push(Outcome::Ran(check));
    }
    outcomes
}

/// Each backend's availability on its own: an unconfigured backend or a missing, nonregular or nonexecutable
/// executable makes only that backend unavailable.
pub(crate) fn backend_checks(backends: &Backends) -> Vec<BackendCheck> {
    [Backend::Mprime, Backend::Ycruncher]
        .into_iter()
        .map(|kind| {
            let (detail, available) = match backends.get(kind) {
                None => (format!("backends.{kind} not configured"), false),
                Some(backend) => match backend.check() {
                    Ok(found) => (found.detail, true),
                    Err(error) => (error_chain(&error), false),
                },
            };
            BackendCheck {
                backend: kind,
                detail,
                available,
            }
        })
        .collect()
}

/// The doctor's `backends` row: every backend's availability, failed when any is unavailable. It is a warning,
/// never a mandatory check.
pub(crate) fn check_backends(backends: &Backends) -> Check {
    let checks = backend_checks(backends);
    let details: Vec<String> = checks
        .iter()
        .map(|check| {
            if check.available {
                format!("{}: {}", check.backend, check.detail)
            } else {
                check.detail.clone()
            }
        })
        .collect();
    Check {
        name: CheckName::Backends.as_str().to_owned(),
        detail: details.join("; "),
        ok: checks.iter().all(|check| check.available),
    }
}

/// The unprivileged account backends run as, or why there is none. A refusal never falls back to
/// root: nothing starts without an identity.
pub(crate) enum Account {
    Resolved {
        name: String,
        identity: BackendIdentity,
    },
    Refused(IdentityError),
}

impl Account {
    pub(crate) fn resolve(name: Option<&str>) -> Self {
        let Some(name) = name else {
            return Self::Refused(IdentityError::NotConfigured);
        };
        match BackendIdentity::lookup(name) {
            Ok(identity) => Self::Resolved {
                name: name.to_owned(),
                identity,
            },
            Err(error) => Self::Refused(error),
        }
    }

    pub(crate) fn identity(&self) -> Option<BackendIdentity> {
        match self {
            Self::Resolved { identity, .. } => Some(*identity),
            Self::Refused(_) => None,
        }
    }

    pub(crate) fn check(&self) -> Check {
        match self {
            Self::Resolved { name, identity } => passed(
                CheckName::BackendUser,
                format!("{name}: uid {} gid {}", identity.uid(), identity.gid()),
            ),
            Self::Refused(error) => failed(CheckName::BackendUser, error_chain(error)),
        }
    }
}

#[cfg(test)]
mod tests;
