use togi_journal::{Activity, Attribution, Decision, Seq, TunerDecision};
use togi_machine::{CoreId, CoreSet, PerCore, TargetOffset};
use togi_requests::groups;

use super::{Round, Shape, actions, shallowest};
use crate::failures::Pending;
use crate::r7::Applied;
use crate::r7::order::lowest_ranked;
use crate::state::Run;
use crate::{DecisionAction, Depth, Profile, Ranking};

/// The deepened core a phase-2 failure is charged to: the round it belongs to, or none for a
/// failure of the confirmation cycle, and the offset the core returns to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Blame {
    failure: Seq,
    round: Option<Shape>,
    blamed: CoreId,
    profile: Profile,
    back: TargetOffset,
}

impl Blame {
    pub(crate) const fn failure(&self) -> Seq {
        self.failure
    }
}

impl Run {
    fn ranked(&self) -> Ranking {
        self.ranking.as_ref().map_or_else(
            || Ranking::ascending(&self.topo),
            |ranked| ranked.ranking.clone(),
        )
    }

    /// The round a failure belongs to: the one its trial or its skipped trial named, or for a
    /// failure of no trial the one in progress when it came.
    fn failed_round(&self, record: &Pending, seq: Seq) -> Option<Shape> {
        let active = self.rounds.active().map(Round::shape);
        match record.round {
            Some(id) => active
                .into_iter()
                .chain(self.rounds.last().copied())
                .find(|shape| shape.id() == id),
            None if record.failure.trial.is_none() => active.filter(|shape| seq > shape.start()),
            None => None,
        }
    }

    /// The core charged for a failure among its deepened loaded cores: the one it names, else
    /// the top request group of a multi-core R7 load, else the one deepened furthest. The
    /// ranking breaks ties, the least preferred core first.
    fn charged(
        &self,
        record: &Pending,
        seq: Seq,
        deepened: CoreSet,
        depth: impl Fn(CoreId) -> i32,
    ) -> Option<CoreId> {
        let failure = &record.failure;
        if let (Attribution::Attributed, Some(core)) = (failure.attribution, failure.core) {
            if deepened.contains(core) {
                return Some(core);
            }
        }
        let ranking = self.ranked();
        if record.class.multi_r7() {
            let requests: Option<PerCore<f64>> = self.r7.open.get(&seq).map(|open| {
                deepened
                    .iter()
                    .filter_map(|core| open.r7.order.value(core).map(|volts| (core, volts)))
                    .collect()
            });
            if let Some(top) = requests.and_then(|requests| groups(&requests).first().copied()) {
                return lowest_ranked(top, &ranking);
            }
        }
        let deepest = deepened.iter().map(&depth).min()?;
        let tied: CoreSet = deepened
            .iter()
            .filter(|&core| depth(core) == deepest)
            .collect();
        lowest_ranked(tied, &ranking)
    }

    /// A failure folded: when it belongs to phase 2 and a deepened core was loaded, it is owed a
    /// blame. Known, carried and shallow failures follow the ordinary rules.
    pub(crate) fn phase2_failure(&mut self, seq: Seq) {
        if !self.phases.in_phase2() {
            return;
        }
        let Some(record) = self.failures.pending(seq).cloned() else {
            return;
        };
        let Some(confirmed) = self.phases.first_confirmed().copied() else {
            return;
        };
        if record.carried || record.failure.known_failure.is_some() {
            return;
        }
        let round = self.failed_round(&record, seq);
        if round.is_none()
            && !(self.phases.confirm_start().is_some() && self.checking.open().is_some())
        {
            return;
        }
        let profile = record.profile;
        let at = |profile: &Profile, core: CoreId| profile.at(core);
        let deepened: CoreSet = record
            .class
            .loaded()
            .iter()
            .filter(|&core| match &round {
                Some(shape) => {
                    shape.deepened(core)
                        && (record.failure.trial.is_some()
                            || at(&profile, core).deeper_than(at(shape.baseline(), core)))
                }
                None => at(&profile, core).deeper_than(at(&confirmed, core)),
            })
            .collect();
        let depth = |core: CoreId| match &round {
            Some(shape) => at(shape.profile(), core).signed() - at(shape.baseline(), core).signed(),
            None => at(&profile, core).signed() - at(&confirmed, core).signed(),
        };
        let Some(blamed) = self.charged(&record, seq, deepened, depth) else {
            return;
        };
        let back = match &round {
            Some(shape) => at(shape.baseline(), blamed),
            None => at(&confirmed, blamed),
        };
        self.phases.push_blame(Blame {
            failure: seq,
            round,
            blamed,
            profile,
            back,
        });
    }

    /// A deepening yield in phase 2 finishes its core and answers no failure.
    pub(crate) fn phase2_yield(&mut self, decision: &TunerDecision) -> bool {
        let yielded = decision.activity == Activity::Deepening
            && decision.decision == Decision::Yield
            && self.phases.phase1_end().is_some();
        if yielded {
            self.phases.finish(decision.core);
        }
        yielded
    }

    /// A deepening backoff in phase 2 answers the failures it cites for every core: no other
    /// decision does.
    pub(crate) fn phase2_backoff(&mut self, decision: &TunerDecision, answered: &[Seq]) {
        let backoff = decision.activity == Activity::Deepening
            && decision.decision == Decision::Backoff
            && self.phases.phase1_end().is_some();
        if !backoff {
            return;
        }
        for &seq in answered {
            self.r7.open.remove(&seq);
        }
        for id in self.topo.all() {
            if self
                .core(id)
                .pending
                .is_some_and(|seq| answered.contains(&seq))
            {
                self.update_core(id, |core| core.pending = None);
            }
        }
    }

    /// Keeps the blames whose failure is still unresolved.
    pub(crate) fn prune_blames(&mut self) {
        let failures = &self.failures;
        self.phases
            .prune_blames(|seq| failures.pending(seq).is_some());
    }

    /// The answer to the first failure owed a blame: the round it ends, then every other mover
    /// returning to its baseline, then the blamed core backing off past the failure point.
    pub(crate) fn phase2_blame_action(&self) -> Option<DecisionAction> {
        let blame = self.phases.blame()?;
        let record = self.failures.pending(blame.failure)?;
        let seq = blame.failure;
        if let Some(shape) = &blame.round {
            if self
                .rounds
                .active()
                .is_some_and(|round| round.id() == shape.id())
            {
                return Some(actions::round_end(
                    shape.id(),
                    false,
                    format!("failure #{seq}: blame goes to the deepened cores"),
                    vec![seq],
                ));
            }
            for &id in self.topo.order() {
                let core = self.core(id);
                let base = shape.baseline().at(id);
                if id == blame.blamed || !shape.deepened(id) || !core.offset.deeper_than(base) {
                    continue;
                }
                return Some(DecisionAction::new(
                    TunerDecision {
                        core: id,
                        activity: Activity::Deepening,
                        decision: Decision::Yield,
                        from_offset: core.offset,
                        to_offset: base,
                        pass: core.pass,
                        failure_point: self.failure_point(id),
                        workloads: Vec::new(),
                        reason: format!(
                            "round {} failed on core {:02}, so core {:02} returns to its last passed offset {}",
                            shape.id(),
                            blame.blamed.get(),
                            id.get(),
                            base.get()
                        ),
                    },
                    vec![seq],
                ));
            }
        }
        let blamed = blame.blamed;
        let core = self.core(blamed);
        let failed = match (record.failure.core, record.failure.offset) {
            (Some(named), Some(offset)) if named == blamed => offset,
            _ => blame.profile.at(blamed),
        };
        let point = self
            .failure_point(blamed)
            .map_or(failed, |old| shallowest(failed, old));
        let to = shallowest(shallowest(core.offset, blame.back), point.shallower_by(1));
        let place: Vec<String> = [
            record.failure.regime.map(|regime| regime.to_string()),
            record
                .failure
                .trial
                .as_ref()
                .map(|trial| format!("trial {trial}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        let origin = match &blame.round {
            Some(shape) => format!(
                "round {}, last passed offset {}",
                shape.id(),
                blame.back.get()
            ),
            None => format!(
                "phase 2 confirmation, last confirmed offset {}",
                blame.back.get()
            ),
        };
        Some(DecisionAction::new(
            TunerDecision {
                core: blamed,
                activity: Activity::Deepening,
                decision: Decision::Backoff,
                from_offset: core.offset,
                to_offset: to,
                pass: core.pass.filter(|pass| pass.shallower_than(point)),
                failure_point: Some(point),
                workloads: Vec::new(),
                reason: format!(
                    "failure #{seq} ({}) charged to deepened core {:02}; {origin}; failure point {}",
                    place.join(" "),
                    blamed.get(),
                    point.get()
                ),
            },
            vec![seq],
        ))
    }
}
