//! Putting back the offsets the machine had before the run.

use togi_machine::{CoreSet, ObservedOffset, PerCore, TargetOffset};
use togi_tuner::{Constraints, FailurePoint};

use crate::error::{Flow, Step};
use crate::session::{Fault, Session};
use crate::write::{self, ProfileRecord, ProfileRequest};

/// The offsets the baseline recorded, kept no deeper than `held`, the offsets the tuner is searching at, and shallower
/// than a failure point by one count. A core the baseline lacks is restored to 0.
pub(crate) fn restore_target(
    baseline: &PerCore<ObservedOffset>,
    held: &PerCore<TargetOffset>,
    failure_points: Option<&PerCore<FailurePoint>>,
    cores: CoreSet,
) -> PerCore<TargetOffset> {
    let zero_point = failure_points.is_some_and(|points| {
        points
            .iter()
            .any(|(_, point)| point.offset() == TargetOffset::ZERO)
    });
    cores
        .iter()
        .map(|core| {
            if zero_point {
                return (core, TargetOffset::ZERO);
            }
            let mut target = baseline.get(core).map_or(TargetOffset::ZERO, |observed| {
                TargetOffset::clamped(observed.0)
            });
            if let Some(held) = held.get(core) {
                target = target.max(held);
            }
            if let Some(point) = failure_points.and_then(|points| points.get(core)) {
                target = target.max(TargetOffset::clamped(point.offset().get() + 1));
            }
            (core, target)
        })
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

impl Session {
    /// Restores the baseline offsets, as bounded by [`restore_target`], when this run touched the machine and the
    /// profile on it is not that one. Without the proof that no workload runs nothing is written. The restoration
    /// cites the baseline.
    pub(crate) fn restore(&mut self, held: &PerCore<TargetOffset>) -> Step<Restoration> {
        let Some((baseline_seq, baseline)) = self.appender.recovery().baseline() else {
            return Ok(Restoration::Unchanged);
        };
        if !self.hardware.engaged() {
            return Ok(Restoration::Unchanged);
        }
        let target = restore_target(
            baseline,
            held,
            self.appender
                .tuner()
                .constraints()
                .map(Constraints::failure_points),
            self.hardware.cores(),
        );
        if self.hardware.profile().as_ref() == Some(&target) {
            return Ok(Restoration::Unchanged);
        }
        let Some(stopped) = self.stopped.as_ref() else {
            return Ok(Restoration::Withheld);
        };
        let mut check = write::checker(self.appender.tuner());
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
            &mut check,
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
            self.fault = Some(Fault::Crashed);
        }
    }
}

#[cfg(test)]
mod tests;
