//! Every core to 0, read back, and reported: the answer to a journal that cannot be written, and the zeroing behind
//! `togi emergency-stop`.
//!
//! The one exception to intent before write, so it is gated three ways. A stale trial scope may still hold a
//! workload until a sweep succeeded, the SMU may not be what togi expects until it was validated, and a workload of
//! this run may still run until teardown is confirmed. Without all three nothing touches the SMU. In a session only
//! a successful sweep makes a [`SweepCleared`] for `Access::gate`. `togi emergency-stop` has no session: it
//! validates the SMU and sweeps itself, and its proofs of both go straight to [`zero_and_read_back`]. The hardware
//! SMU adds one refusal to both: no zero is written before the core slots are mapped.

use std::io::{self, Write};

use togi_machine::{CoreId, CoreSet, Error, ObservedOffset, Smu, SmuValidated, Stopped, Swept};

use crate::error::JournalFailure;

/// A stale-scope sweep of this run succeeded.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SweepCleared(());

impl SweepCleared {
    pub(crate) const fn of(_swept: &Swept) -> Self {
        Self(())
    }
}

/// Why the emergency left the SMU alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Withheld {
    #[error("offsets unchanged before successful trial-scope sweep")]
    NoSweep,
    #[error("offsets unchanged: the SMU interface was not validated")]
    NoValidation,
    #[error("offsets unchanged: the stop of every workload is not confirmed")]
    TeardownUnconfirmed,
    #[error("offsets unchanged: the machine crashed")]
    Crashed,
}

/// The right to zero the SMU in an emergency.
#[derive(Debug)]
pub(crate) struct Access<'a> {
    validated: SmuValidated,
    stopped: &'a Stopped,
}

impl<'a> Access<'a> {
    pub(crate) fn gate(
        sweep: Option<SweepCleared>,
        validated: Option<SmuValidated>,
        stopped: Option<&'a Stopped>,
    ) -> Result<Self, Withheld> {
        sweep.ok_or(Withheld::NoSweep)?;
        let validated = validated.ok_or(Withheld::NoValidation)?;
        let stopped = stopped.ok_or(Withheld::TeardownUnconfirmed)?;
        Ok(Self { validated, stopped })
    }
}

#[derive(Debug)]
pub struct Readback {
    pub core: CoreId,
    pub result: Result<ObservedOffset, Error>,
}

impl Readback {
    fn problem(&self) -> Option<String> {
        match &self.result {
            Ok(ObservedOffset(0)) => None,
            Ok(offset) => Some(format!("core {:02} reads {}", self.core, offset.0)),
            Err(error) => Some(format!(
                "core {:02} unreadable: {}",
                self.core,
                togi_render::error_chain(error)
            )),
        }
    }
}

#[derive(Debug)]
pub struct Zeroed {
    pub zero: Result<(), Error>,
    pub readbacks: Vec<Readback>,
}

impl Zeroed {
    pub fn is_clean(&self) -> bool {
        self.zero.is_ok()
            && self
                .readbacks
                .iter()
                .all(|readback| readback.problem().is_none())
    }

    /// The machine crashed during the emergency, and nothing touched the SMU after it.
    pub fn crashed(&self) -> bool {
        matches!(self.zero, Err(Error::Crashed))
            || self
                .readbacks
                .iter()
                .any(|readback| matches!(readback.result, Err(Error::Crashed)))
    }
}

#[derive(Debug)]
pub enum Response {
    Withheld(Withheld),
    Zeroed(Zeroed),
}

/// What the emergency did, and whether its report reached `report`.
#[derive(Debug)]
pub struct Outcome {
    pub response: Response,
    pub report: io::Result<()>,
}

#[expect(
    clippy::disallowed_methods,
    reason = "the one caller of the zero-only SMU seam, shared by the journal-failure emergency and `togi emergency-stop`"
)]
fn zero(smu: &mut dyn Smu, validated: SmuValidated, stopped: &Stopped) -> Result<(), Error> {
    smu.emergency_zero(validated, stopped)
}

/// Zeroes every core with no intent and reads each core of `cores` back. `stopped` proves no workload runs and
/// `validated` that the SMU is the one togi expects. Nothing is appended or reported here. A crash ends every SMU
/// access at once.
pub fn zero_and_read_back(
    smu: &mut dyn Smu,
    validated: SmuValidated,
    stopped: &Stopped,
    cores: CoreSet,
) -> Zeroed {
    let zero = zero(smu, validated, stopped);
    let mut readbacks = Vec::new();
    if !matches!(zero, Err(Error::Crashed)) {
        for core in cores {
            let result = smu.offset(core);
            let crashed = matches!(result, Err(Error::Crashed));
            readbacks.push(Readback { core, result });
            if crashed {
                break;
            }
        }
    }
    Zeroed { zero, readbacks }
}

/// Answers a failed journal append. Zeroes every core of `cores` without an intent when the gate allows it, reads
/// each back, and writes the original error and what the emergency did to `report`. Appends nothing. A crash ends
/// every SMU access at once.
pub(crate) fn respond(
    original: &JournalFailure,
    gate: Result<Access<'_>, Withheld>,
    smu: &mut dyn Smu,
    cores: CoreSet,
    report: &mut dyn Write,
) -> Outcome {
    let original = togi_render::error_chain(original);
    let access = match gate {
        Ok(access) => access,
        Err(withheld) => {
            let report = writeln!(report, "togi: {original}; {withheld}");
            return Outcome {
                response: Response::Withheld(withheld),
                report,
            };
        }
    };
    let zeroed = zero_and_read_back(smu, access.validated, access.stopped, cores);
    let problems: Vec<String> = zeroed
        .readbacks
        .iter()
        .filter_map(Readback::problem)
        .collect();
    let status = match (problems.is_empty(), zeroed.crashed()) {
        (true, false) => "readback all 0".to_owned(),
        (true, true) => "no readback after the crash".to_owned(),
        (false, false) => format!("readback: {}", problems.join(", ")),
        (false, true) => format!(
            "readback: {}; no readback after the crash",
            problems.join(", ")
        ),
    };
    let report = match &zeroed.zero {
        Ok(()) => writeln!(
            report,
            "togi: {original}; every core set to CO 0 without an intent ({status})"
        ),
        Err(error) => writeln!(
            report,
            "togi: {original}; setting every core to CO 0 without an intent failed: {} ({status})",
            togi_render::error_chain(error)
        ),
    };
    Outcome {
        response: Response::Zeroed(zeroed),
        report,
    }
}

#[cfg(test)]
mod tests;
