use togi_machine::{PerCore, TargetOffset};

use crate::SessionError;
use crate::execute::TunerValues;
use crate::session::{Control, Ended, Outcome, RunInput, Session, Stop};

pub fn run(input: RunInput) -> Ended {
    let mut session = match Session::open(input) {
        Ok(session) => session,
        Err(ended) => return *ended,
    };
    let result = session.startup().and_then(|control| match control {
        Control::Stop(stop) => Ok(stop),
        Control::Continue => session.tune(),
    });
    session.close(result)
}

impl Session {
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
            self.record_drained()?;
            let held = self.held();
            let control = self.reconcile(&held)?;
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

    fn tune(&mut self) -> Result<Stop, SessionError> {
        loop {
            if let Some(stop) = self.check_dead_end()? {
                return Ok(stop);
            }
            let tuner = self.appender.tuner();
            let action = tuner.next()?;
            let profile = tuner.profile();
            let tuned = TunerValues {
                profile: profile.as_ref(),
                profile_seq: tuner.profile_seq(),
                clean_cycles: tuner.clean_cycles(),
                first_result: tuner.first_result(),
            };
            match self.execute(action, &tuned)? {
                Control::Continue => {}
                Control::Stop(stop) => return Ok(stop),
            }
        }
    }

    pub(crate) fn held(&self) -> PerCore<TargetOffset> {
        self.appender
            .tuner()
            .project()
            .cores
            .into_iter()
            .map(|core| (core.core, core.offset))
            .collect()
    }
}

#[cfg(test)]
mod tests;
