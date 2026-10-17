use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use togi_journal::{
    Activity, Attribution, CorePhase, Decision, Event, Failure, HuntOutcome, Member, Payload,
    ResetTarget, Seq, TrialId, TrialIntent, TunerDecision,
};
use togi_machine::{Condition, CoreId, CoreSet, PerCore, Regime, TargetOffset};

use crate::auditor::Seen;
use crate::violation::{Check, Findings};

#[derive(Clone, Copy, Debug)]
struct Point {
    offset: TargetOffset,
    seq: Seq,
    failure: Option<Seq>,
}

#[derive(Debug)]
struct Recorded {
    members: Vec<Member>,
    seq: Seq,
}

#[derive(Clone, Copy, Debug)]
struct Loaded {
    cores: CoreSet,
    regime: Regime,
    condition: Condition,
}

impl Loaded {
    fn of(intent: &TrialIntent) -> Self {
        let mut cores = intent.cores;
        if cores.is_empty() {
            if let Some(core) = intent.core {
                cores.insert(core);
            }
        }
        Self {
            cores,
            regime: intent.regime,
            condition: intent.condition,
        }
    }
}

/// The failure points and combinations a journal has recorded so far, folded from its events alone.
#[derive(Debug, Default)]
pub(crate) struct Marks {
    points: BTreeMap<CoreId, Vec<Point>>,
    combinations: BTreeMap<NonZeroU32, Recorded>,
    queued_resets: BTreeSet<CoreId>,
    hunts: BTreeMap<NonZeroU32, PerCore<TargetOffset>>,
    zero_named: BTreeMap<Seq, CoreId>,
    trials: BTreeMap<TrialId, Loaded>,
    charged_to_named: BTreeSet<Seq>,
    r7_loaded: BTreeMap<Seq, CoreSet>,
}

impl Marks {
    fn shallowest(&self, core: CoreId) -> Option<Point> {
        self.points
            .get(&core)?
            .iter()
            .copied()
            .reduce(|best, point| {
                if point.offset > best.offset {
                    point
                } else {
                    best
                }
            })
    }

    fn mark(&mut self, core: CoreId, offset: Option<TargetOffset>, seq: Seq, failure: Option<Seq>) {
        if let Some(offset) = offset {
            self.points.entry(core).or_default().push(Point {
                offset,
                seq,
                failure,
            });
        }
    }

    fn cancel(&mut self, failure: Seq) {
        for points in self.points.values_mut() {
            points.retain(|point| point.failure != Some(failure));
        }
    }

    /// A decision citing a failure says which core the failure counts against. Charged to its named core, the
    /// failure keeps its point there for good. A hold records none. Charged elsewhere, it leaves the named core only
    /// on grounds the journal records: the decision records its own failure point, it is a phase-2 decision (a
    /// round blame, yield or confirmation revert), or the failure is a multi-core R7 failure whose trial loaded the
    /// charged core. Otherwise the point stands, so a decision blaming the wrong core cannot erase it.
    fn answer(&mut self, failure: Seq, decision: &TunerDecision) {
        let named = self
            .points
            .get(&decision.core)
            .is_some_and(|points| points.iter().any(|point| point.failure == Some(failure)));
        if decision.decision == Decision::Hold {
            self.cancel(failure);
        } else if named {
            self.charged_to_named.insert(failure);
        } else if !self.charged_to_named.contains(&failure) && self.moves(failure, decision) {
            self.cancel(failure);
        }
    }

    fn moves(&self, failure: Seq, decision: &TunerDecision) -> bool {
        decision.failure_point.is_some()
            || decision.activity == Activity::Deepening
            || self
                .r7_loaded
                .get(&failure)
                .is_some_and(|loaded| loaded.contains(decision.core))
    }

    fn reset(&mut self, core: CoreId) {
        self.points.remove(&core);
        self.combinations
            .retain(|_, recorded| recorded.members.iter().all(|member| member.core != core));
        self.queued_resets.remove(&core);
    }

    pub(crate) fn fold(&mut self, event: &Event, seen: &Seen<'_>) {
        let seq = event.seq;
        match &event.payload {
            Payload::SessionCarried(carried) if carried.failure_points => {
                for core in &carried.carried {
                    let offset = core.failure_point.map(|point| point.offset);
                    self.mark(core.core, offset, seq, None);
                }
            }
            Payload::TrialIntent(intent) => {
                self.trials.insert(intent.trial.clone(), Loaded::of(intent));
            }
            Payload::Failure(failure) => self.failure(event, failure, seen),
            Payload::DeadEnd(_) => {
                for cited in &event.cause {
                    if let Some(core) = self.zero_named.get(cited).copied() {
                        self.mark(core, Some(TargetOffset::ZERO), *cited, None);
                    }
                }
            }
            Payload::HuntStart(start) => {
                self.hunts.insert(start.hunt, start.failing);
            }
            Payload::HuntEnd(end) if end.result == HuntOutcome::Culprit && end.cores.len() == 1 => {
                if let Some(core) = end.cores.first() {
                    let offset = self
                        .hunts
                        .get(&end.hunt)
                        .and_then(|failing| failing.get(core));
                    self.mark(core, offset, seq, None);
                }
            }
            Payload::TunerDecision(decision) => {
                if let Some(&cited) = event.cause.first() {
                    self.answer(cited, decision);
                }
                self.mark(decision.core, decision.failure_point, seq, None);
            }
            Payload::CorePhase(phase) => {
                if self.queued_resets.contains(&phase.core) && phase.to == CorePhase::Search {
                    self.reset(phase.core);
                }
                self.mark(phase.core, phase.failure_point, seq, None);
            }
            Payload::CommandReset(reset) => {
                if let ResetTarget::Core(core) = reset.target {
                    self.queued_resets.insert(core);
                }
            }
            Payload::Combination(combination) => {
                self.combinations.insert(
                    combination.combination,
                    Recorded {
                        members: combination.members.clone(),
                        seq,
                    },
                );
            }
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => {}
        }
    }

    fn failure(&mut self, event: &Event, failure: &Failure, seen: &Seen<'_>) {
        let Some(core) = failure.core else { return };
        if failure.attribution != Attribution::Attributed {
            return;
        }
        let intent = failure
            .trial
            .as_ref()
            .and_then(|trial| self.trials.get(trial));
        let (regime, condition) = match (failure.known_failure, intent) {
            (None, Some(intent)) => (
                failure.regime.or(Some(intent.regime)),
                failure.condition.or(Some(intent.condition)),
            ),
            _ => (failure.regime, failure.condition),
        };
        let loaded = self.loaded_cores(failure, seen);
        let multi_core_r7 = regime == Some(Regime::R7)
            && matches!(condition, Some(Condition::Together | Condition::Parked))
            && loaded.len() > 1;
        if multi_core_r7 && failure.offset == Some(TargetOffset::ZERO) {
            self.zero_named.insert(event.seq, core);
            return;
        }
        if multi_core_r7 {
            self.r7_loaded.insert(event.seq, loaded);
        }
        self.mark(core, failure.offset, event.seq, Some(event.seq));
    }

    fn loaded_cores(&self, failure: &Failure, seen: &Seen<'_>) -> CoreSet {
        let by_trial = |trial: Option<&TrialId>| {
            trial
                .and_then(|trial| self.trials.get(trial))
                .map_or_else(CoreSet::default, |loaded| loaded.cores)
        };
        let Some(known) = failure.known_failure else {
            return by_trial(failure.trial.as_ref());
        };
        match seen.get(&known).map(|source| &source.payload) {
            Some(Payload::TrialCarried(carried)) => carried.class.cores,
            Some(Payload::FailureCarried(carried)) => carried.class.cores,
            Some(Payload::TrialEnd(end)) => by_trial(Some(&end.trial)),
            Some(Payload::Failure(source)) => by_trial(source.trial.as_ref()),
            _ => CoreSet::default(),
        }
    }

    /// Reports every recorded failure point or combination that the written cores' offsets reach: `P[c] <= fail[c]`
    /// and every `P[m] <= C[m]`. An offset of 0 deepens nothing, so zeroing is never a violation.
    pub(crate) fn check_profile(
        &self,
        event: &Event,
        profile: &BTreeMap<CoreId, i64>,
        written: &[CoreId],
        findings: &mut Findings,
    ) {
        for &core in written {
            let (Some(point), Some(&offset)) = (self.shallowest(core), profile.get(&core)) else {
                continue;
            };
            if offset < 0 && offset <= point.offset.get() {
                findings.add(
                    event.seq,
                    Check::Avoidance,
                    format!(
                        "core {core} offset {offset} reaches failure point {} recorded at seq {} (tuner.md: P[c] <= fail[c])",
                        point.offset.get(),
                        point.seq
                    ),
                );
            }
        }
        for (id, recorded) in &self.combinations {
            if reaches_combination(profile, written, &recorded.members) {
                findings.add(
                    event.seq,
                    Check::Avoidance,
                    format!(
                        "profile reaches combination C{id} recorded at seq {} (tuner.md: every P[m] <= C[m])",
                        recorded.seq
                    ),
                );
            }
        }
    }
}

fn reaches_combination(
    profile: &BTreeMap<CoreId, i64>,
    written: &[CoreId],
    members: &[Member],
) -> bool {
    let mut deepened = false;
    for member in members {
        match profile.get(&member.core) {
            Some(&offset) if offset <= member.offset.get() => {
                deepened |= offset < 0 && written.contains(&member.core);
            }
            _ => return false,
        }
    }
    deepened
}
