use std::panic::{AssertUnwindSafe, catch_unwind};

use togi_machine::{ContentId, WorkloadId};
use togi_tuner::policy::{Contents, NightQuery, NotReady, State};

use crate::SessionError;
use crate::session::{Control, Ended, Outcome, Panic, RunInput, Session, Stop, message_of};

/// Runs the session. A panic in it is caught only to take the cleanup every exit takes: the run is cancelled, the
/// workload joined, the offsets restored unless that join is unconfirmed, and the journal handed back with the
/// panic in [`Ended::panic`] for the caller to resume.
pub fn run(input: RunInput<'_>) -> Ended {
    let mut session = match Session::open(input) {
        Ok(session) => session,
        Err(ended) => return *ended,
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        session.startup().and_then(|control| match control {
            Control::Stop(stop) => Ok(stop),
            Control::Continue => session.tune(),
        })
    }));
    match result {
        Ok(result) => session.close(result),
        Err(payload) => {
            session.cancel.cancel();
            let message = message_of(&*payload).to_owned();
            let mut ended = session.close(Err(SessionError::Panicked(message)));
            ended.panic = Some(Panic::new(payload));
            ended
        }
    }
}

impl Session<'_> {
    fn startup(&mut self) -> Outcome {
        self.rebuild_state()?;
        for step in [Self::start, Self::recover_prior] {
            let control = step(self)?;
            if control != Control::Continue {
                return Ok(control);
            }
        }
        if !self.same_boot() {
            let control = self.inspect()?;
            if control != Control::Continue {
                return Ok(control);
            }
        }
        for step in [Self::preflight, Self::sweep] {
            let control = step(self)?;
            if control != Control::Continue {
                return Ok(control);
            }
        }
        if self.same_boot() {
            let control = self.recover_same_boot()?;
            if control != Control::Continue {
                return Ok(control);
            }
            let control = self.reconcile()?;
            if control != Control::Continue {
                return Ok(control);
            }
        }
        let control = self.inspect()?;
        if control != Control::Continue {
            return Ok(control);
        }
        self.begin()
    }

    /// The frozen content identity of `workload`, asked of the trial runner once and kept for the run.
    pub(crate) fn content(&mut self, workload: WorkloadId) -> Result<ContentId, SessionError> {
        let key = workload.runs();
        if let Some(content) = self.contents.get(&key) {
            return Ok(content.clone());
        }
        let content = self.trials.content(key)?;
        self.contents.insert(key, content.clone());
        Ok(content)
    }

    /// Asks the night policy `question` with the query of this moment, finding each content it lacks first.
    pub(crate) fn ask<T>(
        &mut self,
        question: impl Fn(&State, &NightQuery<'_>) -> Result<T, NotReady>,
    ) -> Result<T, SessionError> {
        loop {
            let unavailable = self.unavailable_backends();
            let contents = Contents::new(
                self.contents
                    .iter()
                    .map(|(workload, content)| (*workload, content.clone())),
            );
            let query = NightQuery {
                now: self.appender.clock().now(),
                contents: &contents,
                unavailable: &unavailable,
            };
            match question(self.appender.tuner(), &query) {
                Ok(answer) => return Ok(answer),
                Err(NotReady::UnknownContent(workload)) => {
                    self.content(workload)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    fn tune(&mut self) -> Result<Stop, SessionError> {
        loop {
            if let Some(stop) = self.check_dead_end()? {
                return Ok(stop);
            }
            let action = self.ask(State::next)?;
            match self.execute(action)? {
                Control::Continue => {}
                Control::Stop(stop) => return Ok(stop),
            }
        }
    }
}
