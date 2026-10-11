//! Ending a run: the one cleanup every exit takes, whatever ended the run.
//!
//! The workload is joined before a step of the run returns, so a run that reaches the cleanup holds the proof that
//! nothing runs, or holds a fault that says why it does not. Restoration needs that proof and no fault; a latched
//! journal failure is answered by the emergency instead; the `shutdown` is recorded last, and only on a machine whose
//! offsets are back where the run found them.

use togi_journal::{DeadEndCondition, Payload};
use togi_machine::{PerCore, TargetOffset};
use togi_tuner::{DecisionAction, DecisionPayload};

use crate::emergency::{self, Access, Withheld};
use crate::error::{Flow, JournalFailure, SessionError};
use crate::session::{Ended, Fault, Session, Stop};

/// How far the cleanup of a run that may still touch the machine got.
enum Reached {
    /// The machine is as the run found it, or a dead end settled what the restoration could not.
    Settled,
    /// The restoration did not finish: nothing more is recorded about the run's end.
    Unsettled,
}

impl Session {
    /// Ends the run with `run`, the stop it reached or the error that ended it, and hands the journal back.
    ///
    /// `held` is what the tuner still holds per core, which bounds the restoration, and `drained` the decisions it
    /// owes before the run ends. Nothing touches the SMU after a crash or an unconfirmed containment. A restoration
    /// that fails at the SMU turns a clean stop into an SMU dead end and leaves a dead end the run already reached as
    /// it was.
    pub fn close(
        mut self,
        run: Result<Stop, SessionError>,
        held: &PerCore<TargetOffset>,
        drained: Vec<DecisionAction>,
    ) -> Ended {
        let mut errors = Vec::new();
        let mut stop = match run {
            Ok(stop) => Some(stop),
            Err(error) => {
                if error.is_crash() {
                    self.fault = Some(Fault::Crashed);
                }
                errors.push(error);
                None
            }
        };
        if self.fault.is_none() && self.appender.failure().is_none() {
            self.clean_up(&mut stop, held, drained, &mut errors);
        }
        let emergency = self
            .appender
            .failure()
            .cloned()
            .map(|failure| self.answer(&failure));
        self.finish(stop, errors, emergency)
    }

    fn clean_up(
        &mut self,
        stop: &mut Option<Stop>,
        held: &PerCore<TargetOffset>,
        drained: Vec<DecisionAction>,
        errors: &mut Vec<SessionError>,
    ) {
        match self.record_drained(drained) {
            Ok(Some(dead_end)) => *stop = Some(dead_end),
            Ok(None) => {}
            Err(error) => {
                errors.push(error);
                return;
            }
        }
        let reached = if smu_dead_end(stop.as_ref()) {
            Reached::Settled
        } else {
            self.restore_and_settle(stop, held, errors)
        };
        if matches!(reached, Reached::Settled)
            && !self.unreconciled()
            && let Err(error) = self.append_shutdown()
        {
            errors.push(error);
        }
    }

    fn restore_and_settle(
        &mut self,
        stop: &mut Option<Stop>,
        held: &PerCore<TargetOffset>,
        errors: &mut Vec<SessionError>,
    ) -> Reached {
        match self.restore(held) {
            Ok(restoration) if restoration.is_complete() => Reached::Settled,
            Ok(_) => Reached::Unsettled,
            Err(Flow::Fatal(error)) => {
                errors.push(error);
                Reached::Unsettled
            }
            Err(Flow::DeadEnd(condition)) => {
                if matches!(stop, Some(Stop::DeadEnd { .. })) {
                    return Reached::Settled;
                }
                match self.check_dead_end() {
                    Ok(Some(dead_end)) => {
                        *stop = Some(dead_end);
                        Reached::Settled
                    }
                    Ok(None) => {
                        errors.push(SessionError::DeadEndUnrecorded(condition));
                        Reached::Unsettled
                    }
                    Err(error) => {
                        errors.push(error);
                        Reached::Unsettled
                    }
                }
            }
        }
    }

    /// Appends the decisions the tuner owes, in order, before the machine is restored. The first dead end among
    /// them is recorded as the run's dead end and replaces the stop; what follows it stays owed.
    pub(crate) fn record_drained(
        &mut self,
        drained: Vec<DecisionAction>,
    ) -> Result<Option<Stop>, SessionError> {
        for action in drained {
            match action.payload {
                DecisionPayload::DeadEnd(dead_end) => {
                    return self.dead_end(dead_end, &action.cause).map(Some);
                }
                payload => {
                    self.appender.append(payload.into(), &action.cause)?;
                }
            }
        }
        Ok(None)
    }

    fn append_shutdown(&mut self) -> Result<(), SessionError> {
        let Some(mut shutdown) = self.shutdown.take() else {
            return Ok(());
        };
        let boundary = match self.kernel_boundary(None, false, &[]) {
            Ok(boundary) => boundary,
            Err(Flow::Fatal(error)) => return Err(error),
            Err(Flow::DeadEnd(condition)) => {
                return Err(SessionError::DeadEndUnrecorded(condition));
            }
        };
        shutdown.kernel_cursor = boundary.cursor;
        shutdown.kernel_error = boundary.error;
        self.appender.append(Payload::Shutdown(shutdown), &[])?;
        Ok(())
    }

    /// The emergency for a journal that cannot be appended to. A fault withholds it: a crashed machine or one
    /// whose workload may run is not touched.
    fn answer(&mut self, failure: &JournalFailure) -> emergency::Outcome {
        let gate = if self.fault.is_some() {
            Err(Withheld::TeardownUnconfirmed)
        } else {
            Access::gate(self.sweep, self.validated, self.stopped.as_ref())
        };
        emergency::respond(
            failure,
            gate,
            &mut *self.smu,
            self.hardware.cores(),
            &mut *self.report,
        )
    }

    /// The last step of every exit: what the run publishes beyond the journal belongs here, after the final event
    /// and before the journal leaves the session.
    fn finish(
        self,
        stop: Option<Stop>,
        errors: Vec<SessionError>,
        emergency: Option<emergency::Outcome>,
    ) -> Ended {
        Ended {
            stop,
            errors,
            emergency,
            journal: self.appender.into_journal(),
        }
    }
}

fn smu_dead_end(stop: Option<&Stop>) -> bool {
    matches!(
        stop,
        Some(Stop::DeadEnd { dead_end, .. }) if dead_end.condition == DeadEndCondition::Smu
    )
}

#[cfg(test)]
mod tests;
