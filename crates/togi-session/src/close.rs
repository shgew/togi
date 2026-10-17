//! Ending a run: the one cleanup every exit takes, whatever ended the run.
//!
//! The workload is joined before a step of the run returns, so a run that reaches the cleanup holds the proof that
//! nothing runs, or holds a fault that says why it does not. Restoration needs that proof and no fault; a latched
//! journal failure is answered by the emergency instead; the `shutdown` is recorded last, and only on a machine whose
//! offsets are back where the run found them and whose owed decisions were all recorded. A decision that cannot be
//! recorded is an error of the run, but it never leaves the offsets on the machine.

use std::panic::{AssertUnwindSafe, catch_unwind};

use togi_journal::{DeadEndCondition, Payload};
use togi_machine::{PerCore, TargetOffset};
use togi_tuner::DecisionPayload;

use crate::emergency::{self, Access, Withheld};
use crate::error::{Flow, JournalFailure, SessionError};
use crate::session::{Ended, Fault, Panic, Session, Stop, message_of};

/// How far the cleanup of a run that may still touch the machine got.
enum Reached {
    /// The machine is as the run found it, or a dead end settled what the restoration could not.
    Settled,
    /// The restoration did not finish: nothing more is recorded about the run's end.
    Unsettled,
}

impl Session<'_> {
    /// Ends the run with `run`, the stop it reached or the error that ended it, and hands the journal back.
    pub(crate) fn close(mut self, run: Result<Stop, SessionError>) -> Ended {
        let mut errors = Vec::new();
        let mut stop = match run {
            Ok(stop) => Some(stop),
            Err(error) => {
                self.keep(&mut errors, error);
                None
            }
        };
        for error in std::mem::take(&mut self.unconfirmed) {
            self.keep(&mut errors, SessionError::Machine(error));
        }
        let mut panic = None;
        if self.fault.is_none() && self.appender.failure().is_none() {
            let cleanup = catch_unwind(AssertUnwindSafe(|| {
                self.clean_up(&mut stop, &mut errors, &mut panic);
            }));
            if let Err(payload) = cleanup {
                let message = message_of(&*payload).to_owned();
                self.appender.interrupt(&message);
                errors.push(SessionError::Panicked(message));
                panic.get_or_insert_with(|| Panic::new(payload));
            }
        }
        let emergency = self
            .appender
            .failure()
            .cloned()
            .map(|failure| self.answer(&failure));
        let mut ended = self.finish(stop, errors, emergency);
        ended.panic = panic;
        ended
    }

    /// Restores the machine and records the shutdown. A panic out of the drain of a signalled stop is kept as an
    /// error and in `panic` for the caller to resume, and the restoration still runs; no shutdown follows it.
    fn clean_up(
        &mut self,
        stop: &mut Option<Stop>,
        errors: &mut Vec<SessionError>,
        panic: &mut Option<Panic>,
    ) {
        let mut drain_failed = false;
        if matches!(stop, Some(Stop::Signal)) {
            match catch_unwind(AssertUnwindSafe(|| self.record_drained())) {
                Ok(Ok(Some(dead_end))) => *stop = Some(dead_end),
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    self.keep(errors, error);
                    drain_failed = true;
                }
                Err(payload) => {
                    errors.push(SessionError::Panicked(message_of(&*payload).to_owned()));
                    *panic = Some(Panic::new(payload));
                    drain_failed = true;
                }
            }
        }
        if self.fault.is_some() || self.appender.failure().is_some() {
            return;
        }
        let reached = if smu_dead_end(stop.as_ref()) {
            Reached::Settled
        } else {
            let held = self.held();
            self.restore_and_settle(stop, &held, errors)
        };
        if !drain_failed
            && matches!(reached, Reached::Settled)
            && !self.unreconciled()
            && let Err(error) = self.append_shutdown()
        {
            self.keep(errors, error);
        }
    }

    /// Keeps an error of the run; one that says the machine went down latches the crash, so nothing after it
    /// touches the machine, publishes or answers with the emergency as though it had not.
    fn keep(&mut self, errors: &mut Vec<SessionError>, error: SessionError) {
        if error.is_crash() {
            self.fault = Some(Fault::Crashed);
        }
        errors.push(error);
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
                self.keep(errors, error);
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
                        self.keep(errors, error);
                        Reached::Unsettled
                    }
                }
            }
        }
    }

    /// Appends the decisions the tuner owes, in order, before the machine is restored. The first dead end among
    /// them is recorded as the run's dead end and replaces the stop; what follows it stays owed.
    pub(crate) fn record_drained(&mut self) -> Result<Option<Stop>, SessionError> {
        while let Some(action) = self.appender.tuner().drain() {
            if let DecisionPayload::DeadEnd(dead_end) = action.payload {
                return self.dead_end(dead_end, &action.cause).map(Some);
            }
            self.appender.append(action.payload.into(), &action.cause)?;
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
        let gate = match self.fault {
            Some(Fault::Crashed) => Err(Withheld::Crashed),
            Some(Fault::Containment) => Err(Withheld::TeardownUnconfirmed),
            None => Access::gate(self.sweep, self.validated, self.stopped.as_ref()),
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
        mut self,
        stop: Option<Stop>,
        mut errors: Vec<SessionError>,
        mut emergency: Option<emergency::Outcome>,
    ) -> Ended {
        if self.appender.failure().is_none()
            && self.fault != Some(Fault::Crashed)
            && !self.appender.journal().events().is_empty()
            && let Err(error) = self.appender.publish_state()
        {
            errors.push(error.into());
            emergency = self
                .appender
                .failure()
                .cloned()
                .map(|failure| self.answer(&failure));
        }
        errors.extend(self.appender.take_log_failure().map(SessionError::Log));
        let capsule = self
            .appender
            .failure()
            .is_none()
            .then(|| self.appender.capsule());
        Ended {
            stop,
            errors,
            emergency,
            capsule,
            panic: None,
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
