//! Ending a run: the one cleanup every exit takes, whatever ended the run.
//!
//! The workload is joined before a step of the run returns, so a run that reaches the cleanup holds the proof that
//! nothing runs, or holds a fault that says why it does not. Restoration needs that proof and no fault; a latched
//! journal failure is answered by the emergency instead; the `shutdown` is recorded last, and only on a machine whose
//! offsets are back at CO 0.

use std::panic::{AssertUnwindSafe, catch_unwind};

use togi_journal::{DeadEndCondition, Payload};

use crate::emergency::{self, Access};
use crate::error::{Flow, JournalFailure, SessionError};
use crate::session::{Ended, Fault, Panic, Session, Stop, message_of};

/// How far the restoration at the end of a run that may still touch the machine got.
enum Reached {
    /// The machine is as the run found it, or the dead end the run stopped at settled what the restoration could not.
    Settled,
    /// The restoration found dead-end evidence and recorded the dead end, which settles it and replaces the stop.
    DeadEnd(Stop),
    /// The restoration did not finish, for the error if there is one: nothing more is recorded about the run's end.
    Unsettled(Option<SessionError>),
}

/// What the cleanup of a run that may still touch the machine left: the stop the run ends at, which a dead end the
/// cleanup recorded replaces, the errors of the cleanup, and the panic that cut it short.
struct Cleanup {
    stop: Option<Stop>,
    errors: Vec<SessionError>,
    panic: Option<Panic>,
}

impl Session<'_> {
    /// Ends the run with `run`, the stop it reached or the error that ended it, and hands the journal back.
    pub(crate) fn close(mut self, run: Result<Stop, SessionError>) -> Ended {
        let mut errors = Vec::new();
        let mut stop = match run {
            Ok(stop) => Some(stop),
            Err(error) => {
                errors.push(self.keep(error));
                None
            }
        };
        for error in std::mem::take(&mut self.unconfirmed) {
            errors.push(self.keep(SessionError::Machine(error)));
        }
        let panic = if self.lifecycle.fault().is_none() && self.appender.failure().is_none() {
            let cleanup = self.clean_up(stop);
            stop = cleanup.stop;
            errors.extend(cleanup.errors);
            cleanup.panic
        } else {
            None
        };
        let emergency = self
            .appender
            .failure()
            .cloned()
            .map(|failure| self.answer(&failure));
        self.finish(stop, errors, emergency, panic)
    }

    /// Restores the machine to CO 0 and records the shutdown. A panic out of the restoration or the shutdown
    /// interrupts the journal, and a dead end the restoration recorded before it is kept. Either is the cleanup's
    /// panic for the caller to resume.
    fn clean_up(&mut self, stop: Option<Stop>) -> Cleanup {
        let mut cleanup = Cleanup {
            stop,
            errors: Vec::new(),
            panic: None,
        };
        if self.lifecycle.fault().is_some() || self.appender.failure().is_some() {
            return cleanup;
        }
        let stop = cleanup.stop.as_ref();
        let reached = catch_unwind(AssertUnwindSafe(|| {
            if smu_dead_end(stop) {
                Reached::Settled
            } else {
                self.restore_and_settle(stop)
            }
        }));
        match reached {
            Ok(Reached::Settled) => {}
            Ok(Reached::DeadEnd(dead_end)) => cleanup.stop = Some(dead_end),
            Ok(Reached::Unsettled(error)) => {
                cleanup.errors.extend(error);
                return cleanup;
            }
            Err(payload) => {
                self.interrupted(&mut cleanup, payload);
                return cleanup;
            }
        }
        let shutdown = catch_unwind(AssertUnwindSafe(|| {
            if self.unreconciled() {
                Ok(())
            } else {
                self.append_shutdown()
            }
        }));
        match shutdown {
            Ok(Ok(())) => {}
            Ok(Err(error)) => cleanup.errors.push(self.keep(error)),
            Err(payload) => self.interrupted(&mut cleanup, payload),
        }
        cleanup
    }

    /// Interrupts the journal for a panic out of the restoration or the shutdown and keeps it as an error of the
    /// cleanup.
    fn interrupted(&mut self, cleanup: &mut Cleanup, payload: Box<dyn std::any::Any + Send>) {
        let message = message_of(&*payload).to_owned();
        self.appender.interrupt(&message);
        cleanup.errors.push(SessionError::Panicked(message));
        cleanup.panic.get_or_insert_with(|| Panic::new(payload));
    }

    /// Keeps an error of the run; one that says the machine went down latches the crash, so nothing after it
    /// touches the machine, publishes or answers with the emergency as though it had not.
    fn keep(&mut self, error: SessionError) -> SessionError {
        if error.is_crash() {
            self.lifecycle.crash();
        }
        error
    }

    fn restore_and_settle(&mut self, stop: Option<&Stop>) -> Reached {
        match self.restore() {
            Ok(restoration) if restoration.is_complete() => Reached::Settled,
            Ok(_) => Reached::Unsettled(None),
            Err(Flow::Fatal(error)) => Reached::Unsettled(Some(self.keep(error))),
            Err(Flow::DeadEnd(condition)) => {
                if matches!(stop, Some(Stop::DeadEnd { .. })) {
                    return Reached::Settled;
                }
                match self.check_dead_end() {
                    Ok(Some(dead_end)) => Reached::DeadEnd(dead_end),
                    Ok(None) => {
                        Reached::Unsettled(Some(SessionError::DeadEndUnrecorded(condition)))
                    }
                    Err(error) => Reached::Unsettled(Some(self.keep(error))),
                }
            }
        }
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
        emergency::respond(
            failure,
            Access::gate(&self.lifecycle, self.validated),
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
        panic: Option<Panic>,
    ) -> Ended {
        if self.appender.failure().is_none()
            && self.lifecycle.fault() != Some(Fault::Crashed)
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
        let capsule = if self.appender.failure().is_none() {
            match self.appender.capsule() {
                Ok(capsule) => Some(capsule),
                Err(error) => {
                    errors.push(error.into());
                    None
                }
            }
        } else {
            None
        };
        Ended {
            stop,
            errors,
            emergency,
            capsule,
            panic,
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
