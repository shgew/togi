//! Putting back the offsets the machine had before the run.

use togi_machine::{CoreSet, ObservedOffset, PerCore, TargetOffset};
use togi_tuner::{Constraints, FailurePoint};

use crate::error::Step;
use crate::session::Session;
use crate::write::{self, ProfileRecord, ProfileRequest};

/// The offsets the baseline recorded, kept no deeper than `held`, the offsets the tuner is searching at, and shallower
/// than a failure point by one count. A core the baseline lacks is restored to 0.
pub(crate) fn restore_target(
    baseline: &PerCore<ObservedOffset>,
    held: &PerCore<TargetOffset>,
    failure_points: Option<&PerCore<FailurePoint>>,
    cores: CoreSet,
) -> PerCore<TargetOffset> {
    cores
        .iter()
        .map(|core| {
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

impl Session {
    /// Restores the baseline offsets, as bounded by [`restore_target`], when this run touched the machine and the
    /// profile on it is not that one. Without the proof that no workload runs nothing is written. The restoration
    /// cites the baseline.
    pub(crate) fn restore(&mut self, held: &PerCore<TargetOffset>) -> Step<()> {
        let Some((baseline_seq, baseline)) = self.appender.recovery().baseline() else {
            return Ok(());
        };
        if !self.hardware.engaged() {
            return Ok(());
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
            return Ok(());
        }
        let Some(stopped) = self.stopped.as_ref() else {
            return Ok(());
        };
        let mut check = write::checker(self.appender.tuner());
        write::apply_profile(
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
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
