use std::num::NonZeroU32;

use togi_journal::{Activity, Decision, DeepeningRound, RoundEvent, Seq, TunerDecision};
use togi_machine::{Condition, CoreId, CoreSet, PerCore};

use super::context::round_requirement;
use super::{Check, Round};
use crate::failures::{Pending, load_of};
use crate::ledger::carried_clause;
use crate::r7::respond::R7Response;
use crate::state::Run;
use crate::{Action, DecisionAction, Load, TrialPlan, Window, WorkloadChoice};

pub(super) fn round_end(
    round: NonZeroU32,
    passed: bool,
    reason: String,
    cause: Vec<Seq>,
) -> DecisionAction {
    DecisionAction::new(
        DeepeningRound {
            round,
            event: RoundEvent::End,
            base: PerCore::default(),
            base_seq: None,
            target: PerCore::default(),
            profile: PerCore::default(),
            cores: CoreSet::default(),
            ranking: Vec::new(),
            trials: None,
            trial_s: None,
            passed,
            reason: Some(reason),
        },
        cause,
    )
}

fn core_list(cores: &[CoreId]) -> String {
    let names: Vec<String> = cores
        .iter()
        .map(|core| format!("{:02}", core.get()))
        .collect();
    names.join(", ")
}

impl Run {
    /// The open round ends before the backoff of an attributed failure of one of its trials.
    pub(crate) fn round_failure_close(&self, record: &Pending, seq: Seq) -> Option<DecisionAction> {
        let round = self.rounds.active().filter(|_| record.round.is_some())?;
        Some(round_end(
            round.id(),
            false,
            format!("attributed failure #{seq}"),
            vec![seq],
        ))
    }
}

impl Run {
    /// The round this failure queued for a hunt ends before anything else moves.
    pub(crate) fn round_queue_close(&self) -> Option<DecisionAction> {
        let round = self.rounds.active()?;
        let head = self.failures.queue().next()?;
        Some(round_end(
            round.id(),
            false,
            "a failure needs a hunt".to_owned(),
            vec![head],
        ))
    }

    /// The round in progress ends before the located hunt the R7 failure at `failure` needs.
    pub(crate) fn round_locate_close(&self, failure: Seq) -> Option<DecisionAction> {
        let round = self.rounds.active()?;
        Some(round_end(
            round.id(),
            false,
            format!("R7 failure #{failure} needs a located hunt"),
            vec![failure],
        ))
    }

    /// A reset of `core` ends the round in progress.
    pub(crate) fn round_reset_end(&self, core: CoreId, queued: Seq) -> Option<DecisionAction> {
        let round = self.rounds.active()?;
        Some(round_end(
            round.id(),
            false,
            format!("core {:02} was reset", core.get()),
            vec![queued],
        ))
    }

    /// The open round ends before the R7 backoff that answers a failure, citing the failure.
    pub(crate) fn round_before_backoff(&self, response: R7Response) -> Action {
        if let (R7Response::Backoff { decision, .. }, Some(round)) =
            (&response, self.rounds.active())
        {
            if let Some(&failure) = decision.cause.first() {
                return Action::Decide(round_end(
                    round.id(),
                    false,
                    format!("R7 failure #{failure} requires backoff"),
                    vec![failure],
                ));
            }
        }
        response.into_action()
    }

    /// The round the first phase-2 move opens: the cores that move one count toward their solo
    /// limits and those that keep an unverified offset and are checked again in place. The
    /// round's base is the first confirmed profile.
    pub(crate) fn round_start_action(&self) -> Option<Action> {
        let offsets = self.offsets();
        let moves = self.phase2_moves(&offsets, self.phases.passed())?;
        let base = *self.phases.first_confirmed()?;
        let base_seq = self.phases.phase1_end()?;
        let cores: CoreSet = moves.moved.iter().chain(&moves.carried).copied().collect();
        let target = offsets.map(|core, offset| {
            if cores.contains(core) {
                self.core(core).phase.solo_limit().unwrap_or(offset)
            } else {
                offset
            }
        });
        let mut clauses = Vec::new();
        if !moves.moved.is_empty() {
            clauses.push(format!(
                "phase 2: cores {} move one count toward their solo limits",
                core_list(&moves.moved)
            ));
        }
        if !moves.carried.is_empty() {
            clauses.push(format!(
                "cores {} kept their offset from the round that ended unpassed and are checked again",
                core_list(&moves.carried)
            ));
        }
        Some(Action::Decide(DecisionAction::new(
            DeepeningRound {
                round: self.rounds.next_id(),
                event: RoundEvent::Start,
                base: base.offsets(),
                base_seq: Some(base_seq),
                target: target.offsets(),
                profile: moves.profile.offsets(),
                cores,
                ranking: self
                    .ranking
                    .as_ref()
                    .map(|ranked| ranked.ranking.order().to_vec())
                    .unwrap_or_default(),
                trials: Some(u32::from(self.config.needed.get())),
                trial_s: Some(self.config.short_trial.get()),
                passed: false,
                reason: Some(clauses.join("; ")),
            },
            vec![base_seq],
        )))
    }

    /// The round ends unpassed once its profile reaches a constraint; else one missing core move
    /// at a time, in walk order.
    pub(crate) fn round_move_action(&self) -> Option<DecisionAction> {
        let round = self.rounds.active()?;
        if let Some(reached) = self.constraints.reaches(round.profile()).ok().flatten() {
            return Some(round_end(
                round.id(),
                false,
                format!("its profile reaches {reached}"),
                vec![round.start()],
            ));
        }
        self.topo.order().iter().find_map(|&id| {
            let target = round.profile().get(id)?;
            let core = self.core(id);
            if !round.cores().contains(id) || core.offset == target {
                return None;
            }
            let solo = round.target().get(id)?;
            Some(DecisionAction::new(
                TunerDecision {
                    core: id,
                    activity: Activity::Deepening,
                    decision: Decision::Deepen,
                    from_offset: core.offset,
                    to_offset: target,
                    pass: core.pass,
                    failure_point: self.failure_point(id),
                    workloads: Vec::new(),
                    reason: format!(
                        "round {}: one count toward solo limit {}",
                        round.id(),
                        solo.get()
                    ),
                },
                vec![round.start()],
            ))
        })
    }

    /// The first check the round still needs a trial for, or the passed end of the round.
    /// Passes already carried in cite their facts; only the first unmet check is served a
    /// saved retry.
    pub(crate) fn round_check_action(&self) -> Option<Action> {
        let round = self.rounds.active()?;
        let mut cause = vec![round.start()];
        cause.extend(round.sources().iter().copied());
        for check in round.checks() {
            let count = self
                .evidence
                .count(
                    check.class,
                    &check.profile,
                    Window::LiveOrCarried {
                        since: Some(check.since),
                    },
                    round.trials(),
                )
                .unwrap_or_default();
            if count.valid >= u32::from(round.trials().get()) {
                for seq in count.witnesses {
                    if self.evidence.carried_source(seq).is_some() && !cause.contains(&seq) {
                        cause.push(seq);
                    }
                }
                continue;
            }
            return Some(Action::RunTrial(self.round_trial(round, check, cause)));
        }
        let clause = carried_clause(&self.evidence, &cause);
        let reason = clause.strip_prefix("; ").unwrap_or(&clause).to_owned();
        Some(Action::Decide(round_end(round.id(), true, reason, cause)))
    }

    fn round_trial(&self, round: &Round, check: &Check, cause: Vec<Seq>) -> TrialPlan {
        let requirement = round_requirement(round, check);
        if let Some(plan) = self.serve_round_retry(&requirement, vec![round.start()]) {
            return plan;
        }
        let class = check.class;
        TrialPlan {
            regime: class.regime(),
            workload: WorkloadChoice::Frozen(class.workload()),
            load: match check.condition {
                Condition::Alone => load_of(class),
                Condition::Together | Condition::Parked => Load::Many(class.loaded()),
            },
            condition: check.condition,
            activity: Activity::Deepening,
            profile: check.profile,
            duration: class.duration(),
            requirement,
            retry: false,
            cause,
        }
    }
}
