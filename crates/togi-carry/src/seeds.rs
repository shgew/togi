use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;

use togi_defect::{DefectState, Entry};
use togi_facts::SessionFacts;
use togi_journal::{
    Attribution, CarriedFailurePoint, CarriedLimit, Event, HuntOutcome, Outcome, Payload,
    Provenance, Seq, SessionId, TrialId,
};
use togi_machine::{Condition, CoreId, CoreSet, PerCore, Signal, TargetOffset};
use togi_tuner::FailurePoint;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Exclusions {
    seqs: HashSet<Seq>,
    trials: HashSet<TrialId>,
}

impl Exclusions {
    pub(crate) fn excludes(&self, fact: &togi_facts::Fact) -> bool {
        self.seqs.contains(&fact.source.seq)
            || fact
                .source
                .trial
                .as_ref()
                .is_some_and(|trial| self.trials.contains(trial))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.seqs.len() + self.trials.len()
    }
}

pub(crate) struct ExclusionTracker {
    defects: Option<DefectState>,
    failure_trials: HashMap<Seq, TrialId>,
}

impl ExclusionTracker {
    pub(crate) fn new(registry: &'static [Entry]) -> Self {
        Self {
            defects: (!registry.is_empty()).then(|| DefectState::new(registry)),
            failure_trials: HashMap::new(),
        }
    }

    pub(crate) fn fold(&mut self, event: &Event) {
        let Some(defects) = &mut self.defects else {
            return;
        };
        defects.fold(event);
        if let Payload::Failure(failure) = &event.payload
            && let Some(trial) = &failure.trial
        {
            self.failure_trials.insert(event.seq, trial.clone());
        }
    }

    fn excludes(&self, failure: Seq) -> bool {
        self.defects
            .as_ref()
            .is_some_and(|defects| defects.excludes_failure(failure))
    }

    pub(crate) fn finish(self) -> Exclusions {
        let Some(defects) = self.defects else {
            return Exclusions::default();
        };
        let seqs: HashSet<Seq> = defects.excluded_failures().collect();
        let trials = seqs
            .iter()
            .filter_map(|seq| self.failure_trials.get(seq).cloned())
            .collect();
        Exclusions { seqs, trials }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SourceSeeds {
    pub(crate) limits: PerCore<Option<CarriedLimit>>,
    pub(crate) points: PerCore<Option<CarriedFailurePoint>>,
}

impl SourceSeeds {
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.limits.iter().filter(|(_, value)| value.is_some()).count()
            + self.points.iter().filter(|(_, value)| value.is_some()).count()
    }
}

#[derive(Clone, Copy)]
struct IntentInfo {
    core: Option<CoreId>,
    offset: Option<TargetOffset>,
    condition: Condition,
    zero_profile: bool,
}

#[derive(Clone, Copy)]
struct FailedTrial {
    end: Seq,
    intent: Option<IntentInfo>,
}

struct Hunt {
    failure: Seq,
    failing: PerCore<TargetOffset>,
}

#[derive(Clone, Copy)]
enum Proof {
    NotNeeded,
    AtZero {
        immediate: bool,
        failure: Seq,
        end: Option<Seq>,
    },
}

struct FailureCandidate {
    point: CarriedFailurePoint,
    defect_checked: bool,
    proof: Proof,
}

pub(crate) struct SeedFold {
    session: SessionId,
    boundary: Option<SessionId>,
    cores: CoreSet,
    open: HashMap<TrialId, IntentInfo>,
    failed: HashMap<TrialId, FailedTrial>,
    reruns: HashMap<TrialId, Seq>,
    rerun_failed: HashSet<Seq>,
    signals: HashMap<Seq, Signal>,
    failure_end: HashMap<Seq, Seq>,
    hunts: HashMap<NonZeroU32, Hunt>,
    limits: PerCore<Option<CarriedLimit>>,
    failures: PerCore<Option<Vec<FailureCandidate>>>,
}

fn all_zero(profile: &PerCore<TargetOffset>) -> bool {
    !profile.is_empty()
        && profile
            .iter()
            .all(|(_, offset)| offset == TargetOffset::ZERO)
}

impl SeedFold {
    pub(crate) fn new(session: SessionId, boundary: Option<SessionId>) -> Self {
        Self {
            session,
            boundary,
            cores: CoreSet::default(),
            open: HashMap::new(),
            failed: HashMap::new(),
            reruns: HashMap::new(),
            rerun_failed: HashSet::new(),
            signals: HashMap::new(),
            failure_end: HashMap::new(),
            hunts: HashMap::new(),
            limits: PerCore::default(),
            failures: PerCore::default(),
        }
    }

    fn behind_boundary(&self, session: SessionId) -> bool {
        self.boundary.is_some_and(|boundary| session <= boundary)
    }

    fn own(&self, seq: Seq) -> Provenance {
        Provenance {
            session: self.session,
            seq,
        }
    }

    fn offer_limit(&mut self, core: CoreId, limit: CarriedLimit) {
        let deeper = self
            .limits
            .get(core)
            .flatten()
            .is_none_or(|held| limit.offset < held.offset);
        if deeper {
            self.limits.set(core, Some(limit));
        }
    }

    fn offer_failure(&mut self, core: CoreId, candidate: FailureCandidate) {
        if let Some(slot) = self.failures.get_mut(core) {
            slot.get_or_insert_with(Vec::new).push(candidate);
        } else {
            self.failures.set(core, Some(vec![candidate]));
        }
    }

    pub(crate) fn fold(&mut self, event: &Event) {
        match &event.payload {
            Payload::SessionStart(start) => {
                self.cores = start.cores.iter().map(|info| info.core).collect();
            }
            Payload::TrialIntent(intent) => {
                let info = IntentInfo {
                    core: intent.core,
                    offset: intent.offset,
                    condition: intent.condition,
                    zero_profile: all_zero(&intent.profile),
                };
                if intent.condition == Condition::Parked
                    && intent.rerun
                    && intent.hunt.is_none()
                    && info.zero_profile
                    && let Some(&failure) = event.cause.first()
                {
                    self.reruns.insert(intent.trial.clone(), failure);
                }
                self.open.insert(intent.trial.clone(), info);
            }
            Payload::TrialEnd(end) => {
                let intent = self.open.remove(&end.trial);
                if let Some(failure) = self.reruns.remove(&end.trial)
                    && end.outcome == Outcome::Failure
                {
                    self.rerun_failed.insert(failure);
                }
                match end.outcome {
                    Outcome::Pass => {
                        if let Some(IntentInfo {
                            core: Some(core),
                            offset: Some(offset),
                            condition: Condition::Alone,
                            ..
                        }) = intent
                        {
                            let from = self.own(event.seq);
                            self.offer_limit(core, CarriedLimit { offset, from });
                        }
                    }
                    Outcome::Failure => {
                        if let Some(signal) = end.signal {
                            self.signals.insert(event.seq, signal);
                        }
                        self.failed.insert(
                            end.trial.clone(),
                            FailedTrial {
                                end: event.seq,
                                intent,
                            },
                        );
                    }
                    Outcome::Inconclusive => {}
                }
            }
            Payload::Failure(failure) => {
                self.signals.insert(event.seq, failure.signal);
                let failed = failure
                    .trial
                    .as_ref()
                    .and_then(|trial| self.failed.get(trial))
                    .copied();
                if let Some(failed) = failed {
                    self.failure_end.insert(event.seq, failed.end);
                }
                if failure.known_failure.is_none()
                    && failure.attribution == Attribution::Attributed
                    && let (Some(core), Some(offset)) = (failure.core, failure.offset)
                {
                    let proof = if offset == TargetOffset::ZERO {
                        let intent = failed.and_then(|failed| failed.intent);
                        let alone = failure.condition == Some(Condition::Alone)
                            || intent.is_some_and(|i| i.condition == Condition::Alone);
                        let zero_profile = match (failure.profile, intent) {
                            (Some(profile), _) if profile.len() == self.cores.len() => {
                                all_zero(&profile)
                            }
                            (_, Some(intent)) => intent.zero_profile,
                            (Some(profile), None) => all_zero(&profile),
                            (None, None) => false,
                        };
                        Proof::AtZero {
                            immediate: alone || zero_profile,
                            failure: event.seq,
                            end: failed.map(|failed| failed.end),
                        }
                    } else {
                        Proof::NotNeeded
                    };
                    let point = CarriedFailurePoint {
                        offset,
                        from: self.own(event.seq),
                        signal: failure.signal,
                    };
                    self.offer_failure(
                        core,
                        FailureCandidate {
                            point,
                            defect_checked: true,
                            proof,
                        },
                    );
                }
            }
            Payload::HuntStart(start) => {
                self.hunts.insert(
                    start.hunt,
                    Hunt {
                        failure: start.failure,
                        failing: start.failing,
                    },
                );
            }
            Payload::HuntEnd(end) => {
                if end.result == HuntOutcome::Culprit
                    && end.cores.len() == 1
                    && let Some(core) = end.cores.first()
                    && let Some(hunt) = self.hunts.get(&end.hunt)
                    && let Some(offset) = end
                        .members
                        .iter()
                        .find(|member| member.core == core)
                        .map(|member| member.offset)
                        .or_else(|| hunt.failing.get(core))
                    && let Some(&signal) = self.signals.get(&hunt.failure)
                {
                    let proof = if offset == TargetOffset::ZERO {
                        Proof::AtZero {
                            immediate: all_zero(&hunt.failing),
                            failure: hunt.failure,
                            end: self.failure_end.get(&hunt.failure).copied(),
                        }
                    } else {
                        Proof::NotNeeded
                    };
                    let point = CarriedFailurePoint {
                        offset,
                        from: self.own(event.seq),
                        signal,
                    };
                    self.offer_failure(
                        core,
                        FailureCandidate {
                            point,
                            defect_checked: false,
                            proof,
                        },
                    );
                }
            }
            Payload::TrialCarried(carried) => {
                if carried.outcome == Outcome::Failure
                    && let Some(signal) = carried.signal
                {
                    self.signals.insert(event.seq, signal);
                }
            }
            Payload::FailureCarried(carried) => {
                self.signals.insert(event.seq, carried.failure.signal);
            }
            Payload::SessionCarried(carried) => {
                for entry in &carried.carried {
                    if let Some(point) = entry.failure_point
                        && !self.behind_boundary(point.from.session)
                    {
                        self.offer_failure(
                            entry.core,
                            FailureCandidate {
                                point,
                                defect_checked: false,
                                proof: Proof::NotNeeded,
                            },
                        );
                    }
                    if let Some(limit) = entry.candidate_solo_limit
                        && !self.behind_boundary(limit.from.session)
                    {
                        self.offer_limit(entry.core, limit);
                    }
                }
            }
            Payload::CommandReset(reset) => match reset.target {
                togi_journal::ResetTarget::All => {
                    self.limits = PerCore::default();
                    self.failures = PerCore::default();
                }
                togi_journal::ResetTarget::Core(core) => {
                    self.limits.set(core, None);
                    self.failures.set(core, None);
                }
            },
            Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
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
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntGroup(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_)
            | Payload::CorePhase(_)
            | Payload::SessionArchived(_) => {}
        }
    }

    pub(crate) fn finish(
        mut self,
        facts: &SessionFacts,
        exclusions: &ExclusionTracker,
    ) -> SourceSeeds {
        if let Some(activity) = facts.open_trials.last().filter(|activity| {
            facts
                .shutdown
                .is_none_or(|shutdown| activity.seq > shutdown)
        }) {
            let intent = &activity.intent;
            if intent.condition == Condition::Alone
                && let (Some(core), Some(offset)) = (intent.core, intent.offset)
                && offset != TargetOffset::ZERO
                && facts
                    .cutoffs
                    .core(core)
                    .max(facts.cutoffs.all())
                    .is_none_or(|cutoff| activity.seq > cutoff)
            {
                self.offer_failure(
                    core,
                    FailureCandidate {
                        point: CarriedFailurePoint {
                            offset,
                            from: self.own(activity.seq),
                            signal: Signal::Crash,
                        },
                        defect_checked: false,
                        proof: Proof::NotNeeded,
                    },
                );
            }
        }
        let mut points = PerCore::default();
        for (core, candidates) in self.failures.iter_ref() {
            let mut best: Option<CarriedFailurePoint> = None;
            for candidate in candidates.iter().flatten() {
                if candidate.defect_checked && exclusions.excludes(candidate.point.from.seq) {
                    continue;
                }
                let confirmed = match candidate.proof {
                    Proof::NotNeeded => true,
                    Proof::AtZero {
                        immediate,
                        failure,
                        end,
                    } => {
                        immediate
                            || self.rerun_failed.contains(&failure)
                            || end.is_some_and(|end| self.rerun_failed.contains(&end))
                    }
                };
                if confirmed {
                    let point = FailurePoint::new(candidate.point.offset)
                        .merge(best.map(|held| FailurePoint::new(held.offset)));
                    if best.is_none_or(|held| held.offset != point.offset()) {
                        best = Some(candidate.point);
                    }
                }
            }
            if let Some(best) = best {
                points.set(core, Some(best));
            }
        }
        SourceSeeds {
            limits: self.limits,
            points,
        }
    }
}

#[cfg(test)]
pub(crate) fn retained(seeds: &SourceSeeds, exclusions: &Exclusions) -> usize {
    seeds.len() + exclusions.len()
}
