//! Putting back the offsets the machine had before the run.

use togi_machine::{CoreSet, PerCore, TargetOffset};

use crate::error::{Flow, Step};
use crate::session::Session;
use crate::write::{self, ProfileRecord, ProfileRequest};

/// Where a restoration leaves the cores: every one at CO 0. Ruleset 13 starts from a verified all-zero baseline,
/// so zero is the baseline, and the night's profiles are never carried out of the run.
pub(crate) fn restore_target(cores: CoreSet) -> PerCore<TargetOffset> {
    cores
        .iter()
        .map(|core| (core, TargetOffset::ZERO))
        .collect()
}

/// What a restoration left the machine at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Restoration {
    /// The baseline was written back and read back.
    Restored,
    /// Nothing needed writing: the run never touched the machine, no baseline was recorded, or the profile on the
    /// machine already is the target.
    Unchanged,
    /// The profile on the machine is not the target and nothing proves that no workload runs, so nothing was written.
    Withheld,
}

impl Restoration {
    /// Whether the machine holds what the restoration was to leave.
    pub(crate) const fn is_complete(self) -> bool {
        !matches!(self, Self::Withheld)
    }
}

impl Session<'_> {
    /// Restores every core to CO 0 when this run touched the machine and the profile on it is not that one. Without
    /// the proof that no workload runs nothing is written. The restoration cites the baseline.
    pub(crate) fn restore(&mut self) -> Step<Restoration> {
        let Some((baseline_seq, _)) = self.appender.recovery().baseline() else {
            return Ok(Restoration::Unchanged);
        };
        if !self.hardware.engaged() {
            return Ok(Restoration::Unchanged);
        }
        let target = restore_target(self.hardware.cores());
        if self.hardware.profile().as_ref() == Some(&target) {
            return Ok(Restoration::Unchanged);
        }
        let Some(stopped) = self.lifecycle.stopped() else {
            return Ok(Restoration::Withheld);
        };
        let written = write::apply_profile(
            &mut self.appender,
            &mut *self.smu,
            stopped,
            &mut self.hardware,
            ProfileRequest {
                target: &target,
                causes: &[baseline_seq],
                record: ProfileRecord::Restored,
            },
        );
        match written {
            Ok(_) => Ok(Restoration::Restored),
            Err(failure) => {
                let flow = Flow::from(failure);
                self.note_crash(&flow);
                Err(flow)
            }
        }
    }

    /// Remembers that the machine went down under a step, so nothing touches it again.
    pub(crate) fn note_crash(&mut self, flow: &Flow) {
        if let Flow::Fatal(error) = flow
            && error.is_crash()
        {
            self.lifecycle.crash();
        }
    }
}

#[cfg(test)]
mod tests;
