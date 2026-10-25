//! Carrying out what the night policy asks next: a commitment to record, a trial to run or the end of the night.

use togi_journal::{DeadEnd, DeadEndCondition};
use togi_tuner::policy::{Action, Decision, EndReason, PolicyPayload};

use crate::error::{Flow, Step};
use crate::session::{Control, Fault, Outcome, Session, Stop};

impl Session<'_> {
    /// Carries out one action of the policy. A dead end the action's evidence supports ends the run with it.
    pub(crate) fn execute(&mut self, action: Action) -> Outcome {
        let step = match action {
            Action::Decide(decision) => self.decide(decision),
            Action::RunTrial(plan) => self.run(&plan),
            Action::EndNight(EndReason::DeadEnd) => {
                Ok(self.shut_down(Stop::Night(EndReason::DeadEnd)))
            }
            Action::EndNight(EndReason::Unavailable) => self.unavailable_dead_end(),
        };
        let outcome = self.settle(step);
        if let Err(error) = &outcome
            && error.is_crash()
        {
            self.lifecycle.latch(Fault::Crashed);
        }
        outcome
    }

    /// Every cell the night could run needs an excluded backend: the evidence-backed dead end when one stands,
    /// otherwise a no-evidence dead end citing the inconclusive streaks that excluded the backends.
    fn unavailable_dead_end(&mut self) -> Step<Control> {
        if let Some(stop) = self.check_dead_end()? {
            return Ok(Control::Stop(stop));
        }
        let exhausted = self
            .appender
            .recovery()
            .exhausted_backends(self.inconclusive_threshold());
        let causes: Vec<_> = exhausted
            .iter()
            .flat_map(|(_, streak)| streak.iter().copied())
            .collect();
        let names: Vec<String> = exhausted
            .iter()
            .map(|(backend, streak)| format!("{backend} {} times", streak.len()))
            .collect();
        let dead_end = DeadEnd {
            condition: DeadEndCondition::NoEvidence,
            core: None,
            detail: format!(
                "every runnable cell needs an exhausted backend; inconclusive in a row: {}",
                names.join(", ")
            ),
            action: None,
        };
        Ok(Control::Stop(self.dead_end(dead_end, &causes)?))
    }

    /// Appends the commitment with the events it cites. A dead end goes through the dead-end path instead, which
    /// records it, leaves the tuning boot and ends the run.
    fn decide(&mut self, decision: Decision) -> Step<Control> {
        let Decision { payload, causes } = decision;
        if let PolicyPayload::DeadEnd(dead_end) = payload {
            return Ok(Control::Stop(self.dead_end(dead_end, &causes)?));
        }
        if self.cancel.is_cancelled() {
            return Ok(self.shut_down(Stop::Signal));
        }
        self.appender.append(payload.into(), &causes)?;
        Ok(Control::Continue)
    }
}

pub(crate) fn is_crash_flow(flow: &Flow) -> bool {
    matches!(flow, Flow::Fatal(error) if error.is_crash())
}
