use std::error::Error;
use std::fmt;

use togi_machine::{Check, error_chain};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckName {
    Root,
    Cpu,
    RyzenSmu,
    Readback,
    PmTable,
    SlotMapping,
    Backends,
    BackendUser,
    SystemdRun,
    Watchdog,
    BiosContext,
}

impl CheckName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Cpu => "cpu",
            Self::RyzenSmu => "ryzen_smu",
            Self::Readback => "readback",
            Self::PmTable => "pm_table",
            Self::SlotMapping => "slot_mapping",
            Self::Backends => "backends",
            Self::BackendUser => "backend_user",
            Self::SystemdRun => "systemd_run",
            Self::Watchdog => "watchdog",
            Self::BiosContext => "bios_context",
        }
    }
}

impl fmt::Display for CheckName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a check was not made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Skip {
    NeedsRoot,
    NeedsIdentity,
    NeedsOtherChecks,
}

impl fmt::Display for Skip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NeedsRoot => "needs root",
            Self::NeedsIdentity => "needs passing cpu and ryzen_smu checks",
            Self::NeedsOtherChecks => "needs every other check to pass",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Skipped {
    pub name: CheckName,
    pub reason: Skip,
}

impl Skipped {
    /// A check that was not made did not pass: the reason stands in as its failed detail.
    pub fn into_check(self) -> Check {
        failed(self.name, self.reason.to_string())
    }
}

pub(crate) fn passed(name: CheckName, detail: impl Into<String>) -> Check {
    Check {
        name: name.as_str().to_owned(),
        detail: detail.into(),
        ok: true,
    }
}

pub(crate) fn failed(name: CheckName, detail: impl Into<String>) -> Check {
    Check {
        name: name.as_str().to_owned(),
        detail: detail.into(),
        ok: false,
    }
}

pub(crate) fn outcome<E: Error>(name: CheckName, result: Result<String, E>) -> Check {
    match result {
        Ok(detail) => passed(name, detail),
        Err(error) => failed(name, error_chain(&error)),
    }
}
