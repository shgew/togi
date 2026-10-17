//! Carrying out what the tuner asks next: a decision to record, a ranking to read, or a trial to run.

use togi_journal::{CycleEvent, HostRanking, Payload, RoundEvent, Seq};
use togi_machine::{CoreId, CoreRank, PerCore};
use togi_tuner::{Action, DecisionAction, DecisionPayload, Profile};

use crate::error::{Flow, SessionError, Step};
use crate::session::{Control, Fault, Outcome, Session, Stop, StopPolicy};

/// What only the tuner knows when it asks for an action: the profile it stands at, the event that profile came from,
/// the clean checking cycles it has completed and whether the BIOS profile is confirmed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TunerValues<'a> {
    pub profile: Option<&'a Profile>,
    pub profile_seq: Option<Seq>,
    pub clean_cycles: u32,
    pub first_result: bool,
}

impl StopPolicy {
    /// The stop `payload` is where a run that asked for one ends, if it is: only the start of a checking cycle or
    /// round, never a step of a confirmation under way.
    pub(crate) fn reached(
        self,
        payload: &DecisionPayload,
        clean_cycles: u32,
        first_result: bool,
    ) -> Option<Stop> {
        let first = self.first_result && first_result;
        match payload {
            DecisionPayload::CheckingCycle(cycle) if cycle.event == CycleEvent::Start => {
                if self
                    .cycles
                    .is_some_and(|cycles| clean_cycles >= cycles.get())
                {
                    Some(Stop::Cycles)
                } else {
                    first.then_some(Stop::FirstResult)
                }
            }
            DecisionPayload::DeepeningRound(round) if round.event == RoundEvent::Start => {
                first.then_some(Stop::FirstResult)
            }
            DecisionPayload::CheckingCycle(_)
            | DecisionPayload::DeepeningRound(_)
            | DecisionPayload::Failure(_)
            | DecisionPayload::TunerWarning(_)
            | DecisionPayload::DeadEnd(_)
            | DecisionPayload::CorePhase(_)
            | DecisionPayload::TunerDecision(_)
            | DecisionPayload::ProfileChange(_)
            | DecisionPayload::CheckingStep(_)
            | DecisionPayload::CheckingChain(_)
            | DecisionPayload::HuntStart(_)
            | DecisionPayload::HuntGroup(_)
            | DecisionPayload::HuntEnd(_)
            | DecisionPayload::HuntSkipped(_)
            | DecisionPayload::Combination(_) => None,
        }
    }
}

impl Session<'_> {
    /// Carries out one action of the tuner. A dead end the action's evidence supports ends the run with it.
    pub(crate) fn execute(&mut self, action: Action, tuned: &TunerValues<'_>) -> Outcome {
        let step = match action {
            Action::Decide(decision) => self.decide(decision, tuned),
            Action::ReadRanking => self.rank(),
            Action::RunTrial(plan) => self.run(&plan, tuned),
        };
        let outcome = self.settle(step);
        if let Err(error) = &outcome
            && error.is_crash()
        {
            self.lifecycle.latch(Fault::Crashed);
        }
        outcome
    }

    fn decide(&mut self, decision: DecisionAction, tuned: &TunerValues<'_>) -> Step<Control> {
        let DecisionAction { payload, cause } = decision;
        if let DecisionPayload::DeadEnd(dead_end) = payload {
            return Ok(Control::Stop(self.dead_end(dead_end, &cause)?));
        }
        if let Some(stop) = self
            .policy
            .reached(&payload, tuned.clean_cycles, tuned.first_result)
        {
            return Ok(self.shut_down(stop));
        }
        if self.cancel.is_cancelled() {
            return Ok(self.shut_down(Stop::Signal));
        }
        self.appender.append(payload.into(), &cause)?;
        Ok(Control::Continue)
    }

    /// Records the cores in the order the host ranks them, best first, ties by core id. A ranking the host cannot
    /// give, or one that tells the cores apart by nothing, is recorded with what is wrong and the cores by id.
    fn rank(&mut self) -> Step<Control> {
        if self.cancel.is_cancelled() {
            return Ok(self.shut_down(Stop::Signal));
        }
        let (ranks, error) = match self.host.ranking() {
            Ok(ranks) => (ranks, None),
            Err(partial) if matches!(partial.error, togi_machine::Error::Crashed) => {
                return Err(SessionError::from(partial.error).into());
            }
            Err(partial) => (
                partial.value,
                Some(togi_render::error_chain(&partial.error)),
            ),
        };
        let values: PerCore<i64> = ranks.iter().map(|rank| (rank.core, rank.value)).collect();
        let ranking = ranking(&self.topology, ranks, error);
        let payload = HostRanking {
            ranking: ranking.order,
            values,
            detail: ranking.detail,
        };
        self.appender.append(Payload::HostRanking(payload), &[])?;
        Ok(Control::Continue)
    }
}

struct Ranked {
    order: Vec<CoreId>,
    detail: Option<String>,
}

fn ranking(
    topology: &[togi_machine::CoreInfo],
    mut ranks: Vec<CoreRank>,
    error: Option<String>,
) -> Ranked {
    let by_id = || topology.iter().map(|info| info.core).collect::<Vec<_>>();
    if let Some(error) = error {
        return Ranked {
            order: by_id(),
            detail: Some(error),
        };
    }
    let mut named: Vec<CoreId> = ranks.iter().map(|rank| rank.core).collect();
    named.sort_unstable();
    let expected = by_id();
    if named != expected {
        return Ranked {
            order: expected,
            detail: Some(format!(
                "ranking has {} values for {} cores",
                ranks.len(),
                topology.len()
            )),
        };
    }
    let first = ranks.first().map(|rank| rank.value);
    if let Some(value) = first
        && ranks.iter().all(|rank| rank.value == value)
    {
        return Ranked {
            order: expected,
            detail: Some(format!("every core ranks {value}")),
        };
    }
    ranks.sort_by(|a, b| b.value.cmp(&a.value).then(a.core.cmp(&b.core)));
    Ranked {
        order: ranks.into_iter().map(|rank| rank.core).collect(),
        detail: None,
    }
}

pub(crate) fn is_crash_flow(flow: &Flow) -> bool {
    matches!(flow, Flow::Fatal(error) if error.is_crash())
}

#[cfg(test)]
mod tests;
