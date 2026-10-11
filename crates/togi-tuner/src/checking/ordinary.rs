use togi_journal::{Activity, CorePhase, Decision, HuntSkipped, Seq, TunerDecision};
use togi_machine::{Condition, CoreId, TargetOffset};

use crate::failures::Pending;
use crate::state::Run;
use crate::zero::AtZero;
use crate::{Action, DecisionAction, Depth};

impl Run {
    /// What the attributed failure of the first core with one pending decides: a search core
    /// backs off or checks its solo limit, a failure at CO 0 goes to the zero diagnostic, any
    /// other backs the core off.
    pub(crate) fn pending_core_action(&self) -> Option<Action> {
        let (id, seq) = self.topo.order().iter().find_map(|&id| {
            let core = self.core(id);
            core.pending.map(|seq| (id, seq))
        })?;
        let core = self.core(id);
        let record = self.failures.pending(seq);
        if core.phase == Some(CorePhase::Search) {
            let failed = record.and_then(Pending::offset).unwrap_or(core.offset);
            return Some(Action::Decide(DecisionAction::new(
                self.search_failure(id, failed),
                vec![seq],
            )));
        }
        let record = record?;
        let failed = record.failure.offset?;
        if failed == TargetOffset::ZERO {
            let head = format!("core {:02} failed at CO 0", id.get());
            return match self.at_zero(seq, record, id, &head, vec![seq]) {
                AtZero::DeadEnd(decision) => Some(Action::Decide(*decision)),
                AtZero::Rerun(plan) => Some(Action::RunTrial(*plan)),
                AtZero::Released => None,
            };
        }
        Some(Action::Decide(
            self.attributed_backoff(id, seq, record, failed),
        ))
    }

    /// Records the failure point at the failed offset and steps the core one count shallower than
    /// it failed, never deeper than where it already is.
    fn attributed_backoff(
        &self,
        id: CoreId,
        seq: Seq,
        record: &Pending,
        failed: TargetOffset,
    ) -> DecisionAction {
        let core = self.core(id);
        let failure = &record.failure;
        let point = self.failure_point(id).map_or(failed, |old| {
            if old.shallower_than(failed) {
                old
            } else {
                failed
            }
        });
        let target = failed.shallower_by(1);
        let to = if target.shallower_than(core.offset) {
            target
        } else {
            core.offset
        };
        let pass = core.pass.filter(|pass| pass.shallower_than(point));
        let activity = if failure.condition == Some(Condition::Parked) {
            Activity::Hunt
        } else {
            Activity::Checking
        };
        let place: Vec<String> = [
            failure.condition.map(|condition| format!("in {condition}")),
            failure.regime.map(|regime| regime.to_string()),
            failure.trial.as_ref().map(|trial| format!("trial {trial}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut clauses = vec![
            format!(
                "backed off: attributed {} {}",
                failure.signal,
                place.join(" ")
            ),
            format!("failure point {}", point.get()),
        ];
        if let (Some(_), Some(known)) = (failure.known_failure, &failure.reason) {
            clauses.push(known.clone());
        }
        if to == core.offset {
            clauses.push("already shallower".to_owned());
        }
        let reason = clauses.join("; ");
        DecisionAction::new(
            TunerDecision {
                core: id,
                activity,
                decision: Decision::Backoff,
                from_offset: core.offset,
                to_offset: to,
                pass,
                failure_point: Some(point),
                workloads: Vec::new(),
                reason,
            },
            vec![seq],
        )
    }

    /// The queued unattributed failure whose profile already reaches a recorded failure point or
    /// combination is not hunted: nothing a hunt could find would add to what is known.
    pub(crate) fn hunt_skipped_decision(&self) -> Option<DecisionAction> {
        let seq = self.failures.queue().next()?;
        let record = self.failures.pending(seq)?;
        let reached = self.constraints.reaches(&record.profile).ok().flatten()?;
        Some(DecisionAction::new(
            HuntSkipped {
                failure: seq,
                reason: format!("its profile reaches {reached}"),
            },
            vec![seq],
        ))
    }

    pub(crate) fn hunt_skipped(&mut self, event: &togi_journal::Event, skipped: &HuntSkipped) {
        self.later_hunt_skipped(event, skipped);
        self.failures.retire(skipped.failure);
    }
}
