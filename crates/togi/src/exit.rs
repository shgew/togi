//! The process exit statuses of `docs/spec/runtime.md` (Exit codes), as one type every command
//! returns and `main` turns into the process status.

use std::process::ExitCode;

use togi_journal::DeadEndCondition;

/// A process exit status. `doctor` reports with the same values: [`Exit::Ok`] when no check
/// failed, [`Exit::Preflight`] when one did, [`Exit::Incompatible`] for a refused journal,
/// [`Exit::Locked`] when another togi process holds the host lock, [`Exit::Usage`] for a
/// configuration error and [`Exit::Error`] otherwise.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Exit {
    /// Stopped cleanly, or a command succeeded.
    Ok,
    /// Any other failure; the error is on stderr.
    Error,
    /// A usage or configuration error.
    Usage,
    /// The host lock or the journal is held by another writer.
    Locked,
    /// Dead end `failure_at_zero`.
    FailureAtZero,
    /// Dead end `smu`.
    Smu,
    /// Dead end `no_evidence`.
    NoEvidence,
    /// Dead end `boot_loop`.
    BootLoop,
    /// Dead end `containment`.
    Containment,
    /// Dead end `preflight`.
    Preflight,
    /// The journal's schema, ruleset or evidence epoch is newer than this build's.
    Incompatible,
    /// Dead end `defect`.
    Defect,
    /// Dead end `thermal_trip`.
    ThermalTrip,
}

impl Exit {
    /// Every status, in code order.
    pub const ALL: [Self; 13] = [
        Self::Ok,
        Self::Error,
        Self::Usage,
        Self::Locked,
        Self::FailureAtZero,
        Self::Smu,
        Self::NoEvidence,
        Self::BootLoop,
        Self::Containment,
        Self::Preflight,
        Self::Incompatible,
        Self::Defect,
        Self::ThermalTrip,
    ];

    /// The status the process exits with.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Error => 1,
            Self::Usage => 2,
            Self::Locked => 3,
            Self::FailureAtZero => 10,
            Self::Smu => 11,
            Self::NoEvidence => 12,
            Self::BootLoop => 13,
            Self::Containment => 14,
            Self::Preflight => 15,
            Self::Incompatible => 16,
            Self::Defect => 17,
            Self::ThermalTrip => 18,
        }
    }

    /// The status `run` exits with when the session ends in this dead end.
    #[must_use]
    pub const fn dead_end(condition: DeadEndCondition) -> Self {
        match condition {
            DeadEndCondition::FailureAtZero => Self::FailureAtZero,
            DeadEndCondition::Smu => Self::Smu,
            DeadEndCondition::NoEvidence => Self::NoEvidence,
            DeadEndCondition::BootLoop => Self::BootLoop,
            DeadEndCondition::Containment => Self::Containment,
            DeadEndCondition::Preflight => Self::Preflight,
            DeadEndCondition::Defect => Self::Defect,
            DeadEndCondition::ThermalTrip => Self::ThermalTrip,
        }
    }
}

impl From<DeadEndCondition> for Exit {
    fn from(condition: DeadEndCondition) -> Self {
        Self::dead_end(condition)
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit.code())
    }
}

#[cfg(test)]
mod tests;
