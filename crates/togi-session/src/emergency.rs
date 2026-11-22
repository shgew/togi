//! Every core to 0, read back, and reported: the answer to a journal that cannot be written, and the zeroing behind
//! `togi emergency-stop`.
//!
//! The one exception to intent before write, so it is gated three ways. A stale trial scope may still hold a
//! workload until a sweep succeeded, the SMU may not be what togi expects until it was validated, and a workload of
//! this run may still run until teardown is confirmed. Without all three nothing touches the SMU. In a session only
//! a successful sweep makes a [`SweepCleared`], which only the swept [`Lifecycle`] holds for `Access::gate`. `togi
//! emergency-stop` has no session: it validates the SMU and sweeps itself, and its proofs of both go straight to
//! [`zero_and_read_back`]. The hardware SMU adds one refusal to both: no zero is written before the core slots are
//! mapped.

use std::io::{self, Write};

use togi_machine::{CoreSet, Smu, SmuValidated, Stopped, Swept, Zeroed, zero_and_read_back};

use crate::error::JournalFailure;
use crate::session::{Fault, Lifecycle};

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
    /// The right the run's `lifecycle` and SMU validation give: a fault withholds it first, then a missing sweep,
    /// validation or proof that no workload runs.
    pub(crate) fn gate(
        lifecycle: &'a Lifecycle,
        validated: Option<SmuValidated>,
    ) -> Result<Self, Withheld> {
        let swept = match lifecycle {
            Lifecycle::Faulted(Fault::Crashed) => return Err(Withheld::Crashed),
            Lifecycle::Faulted(Fault::Containment) => return Err(Withheld::TeardownUnconfirmed),
            Lifecycle::Unswept => return Err(Withheld::NoSweep),
            Lifecycle::Swept(swept) => swept,
        };
        let validated = validated.ok_or(Withheld::NoValidation)?;
        let stopped = swept
            .stopped
            .as_ref()
            .ok_or(Withheld::TeardownUnconfirmed)?;
        Ok(Self { validated, stopped })
    }
}

#[derive(Debug)]
pub enum Response {
    Withheld(Withheld),
    Zeroed(Zeroed),
}

/// What the emergency did, and whether its report reached `report`.
#[derive(Debug)]
#[must_use = "what the emergency did to the SMU, and whether its report was written, has to be reported"]
pub struct Outcome {
    pub response: Response,
    pub report: io::Result<()>,
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
    let report = writeln!(report, "togi: {original}; {zeroed}");
    Outcome {
        response: Response::Zeroed(zeroed),
        report,
    }
}

#[cfg(test)]
mod tests;
