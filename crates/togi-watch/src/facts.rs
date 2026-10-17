//! What the fold remembers beside the journal's and the tuner's states: the facts only the event stream says,
//! such as when a trial started, how a group failed, which backoff cleared which combination.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::num::NonZeroU32;
use std::time::Duration;

use jiff::Timestamp;
use togi_journal::{
    Activity, Attribution, DeadEndCondition, Event, Failure, HuntStart, Outcome, Payload,
    ResetTarget, Seq, State as JournalState, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{
    BootId, Condition, CoreId, CoreSet, ObservedOffset, Regime, Signal, TargetOffset,
};
use togi_tuner::{CombinationId, State as TunerState, TrialHistory};

use crate::history::{History, LOG_LIMIT, LogEntry};
use crate::model::{
    CoreEvidence, Count, DeadEndView, FailureEvidence, FailureSource, HuntShape, OffsetDecision,
    SessionView, StopView, TrialEndView,
};
use crate::tuning::TuningClock;
use crate::words::{tag_word, words};

#[derive(Debug, Clone)]
pub(crate) struct Current {
    pub(crate) intent: TrialIntent,
    pub(crate) boot: BootId,
    pub(crate) boot_ended: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct FailureFacts {
    pub(crate) at: Option<Timestamp>,
    pub(crate) signal: Option<Signal>,
    pub(crate) core: Option<CoreId>,
    pub(crate) known: bool,
    pub(crate) carried: bool,
    pub(crate) regime: Option<Regime>,
    pub(crate) source: FailureSource,
}

/// What a trial works on: the step, hunt, round, core and offset its intent names. A failure recorded while the
/// operation stands is the current one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Operation {
    cycle: Option<NonZeroU32>,
    step: Option<NonZeroU32>,
    hunt: Option<NonZeroU32>,
    round: Option<NonZeroU32>,
    core: Option<CoreId>,
    offset: Option<TargetOffset>,
}

impl Operation {
    fn of(intent: &TrialIntent) -> Self {
        Self {
            cycle: intent.cycle,
            step: intent.step,
            hunt: intent.hunt,
            round: intent.round,
            core: intent.core,
            offset: intent.offset,
        }
    }
}

fn failure_source(failure: &Failure) -> FailureSource {
    match (failure.attribution, failure.regime, failure.condition) {
        (Attribution::Unattributed, Some(Regime::R7), _) => FailureSource::R7Charged,
        (Attribution::Attributed, _, Some(Condition::Alone)) => FailureSource::Alone,
        (Attribution::Attributed | Attribution::Unattributed, _, _) => FailureSource::Named,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct HuntStartFacts {
    pub(crate) at: Timestamp,
    pub(crate) named: bool,
    pub(crate) live: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ComboFacts {
    pub(crate) hunt: u32,
    pub(crate) fallback: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct RecoverFacts {
    pub(crate) crash_at: Timestamp,
    pub(crate) reset: Option<togi_machine::ResetKind>,
    pub(crate) intent: Option<TrialIntent>,
    pub(crate) end: Option<TrialEndView>,
}

#[derive(Debug, Default)]
pub(crate) struct Facts {
    pub(crate) session: Option<SessionView>,
    pub(crate) intents: HashMap<TrialId, TrialIntent>,
    pub(crate) starts: HashMap<TrialId, Timestamp>,
    pub(crate) ends: HashMap<TrialId, TrialEndView>,
    pub(crate) group_signals: HashMap<(u32, u32), Signal>,
    pub(crate) applied: BTreeMap<CoreId, ObservedOffset>,
    pub(crate) tuned: BTreeMap<CoreId, TargetOffset>,
    pub(crate) current: Option<Current>,
    pub(crate) steps: HashMap<TrialId, u32>,
    pub(crate) counts: HashMap<TrialId, Count>,
    pub(crate) failed: HashMap<TrialId, TrialHistory>,
    pub(crate) by_failure: HashMap<Seq, FailureFacts>,
    pub(crate) backs: BTreeMap<CoreId, Option<CombinationId>>,
    pub(crate) sources: HashMap<Seq, CombinationId>,
    pub(crate) combination_seqs: HashMap<Seq, CombinationId>,
    pub(crate) combos: BTreeMap<CombinationId, ComboFacts>,
    pub(crate) probes: HashSet<u32>,
    pub(crate) stopped: bool,
    pub(crate) restored: bool,
    pub(crate) configs: u32,
    pub(crate) carrying: bool,
    pub(crate) check_hunts: HashMap<(u32, u32), Vec<u32>>,
    pub(crate) cycle_steps: HashMap<u32, Vec<Regime>>,
    pub(crate) cycle_ends: HashMap<Seq, (u32, Timestamp)>,
    pub(crate) hunt_starts: HashMap<u32, HuntStartFacts>,
    pub(crate) groups: HashMap<(u32, u32), togi_journal::HuntGroup>,
    pub(crate) carried: BTreeSet<Seq>,
    pub(crate) shapes: BTreeMap<u32, HuntShape>,
    pub(crate) hunt_started: BTreeMap<u32, Timestamp>,
    pub(crate) queued: BTreeMap<CoreId, Seq>,
    pub(crate) evidence: BTreeMap<CoreId, CoreEvidence>,
    pub(crate) operation: Option<Operation>,
    pub(crate) failed_in: BTreeMap<CoreId, Operation>,
    pub(crate) failures: u32,
    pub(crate) crashes: u32,
    pub(crate) last_failure: Option<Timestamp>,
    pub(crate) last: Option<TrialEndView>,
    pub(crate) recover: Option<RecoverFacts>,
    pub(crate) stop: Option<StopView>,
    pub(crate) dead_end: Option<DeadEndView>,
    pub(crate) history: History,
    pub(crate) tuning: TuningClock,
    pub(crate) log: Vec<LogEntry>,
    pub(crate) log_total: usize,
    /// The profile the latest profile event in the log put in force, which the next one is shown against.
    pub(crate) logged_profile: BTreeMap<CoreId, TargetOffset>,
}

pub(crate) fn secs(seconds: u32) -> Duration {
    Duration::from_secs(u64::from(seconds))
}

pub(crate) fn trial_cores(intent: &TrialIntent) -> CoreSet {
    match intent.core {
        Some(core) => std::iter::once(core).collect(),
        None => intent.cores,
    }
}

impl Facts {
    /// Takes one event, after both states took it. The tuner state has folded `event`, so a trial end finds its
    /// requirement in [`TunerState::trial_history`].
    pub(crate) fn fold(&mut self, event: &Event, journal: &JournalState, tuner: &TunerState) {
        self.tuning.fold(event);
        self.clears_combination(event);
        if let Some(current) = &mut self.current {
            if !event.boot.as_str().is_empty()
                && !current.boot.as_str().is_empty()
                && event.boot != current.boot
            {
                current.boot_ended = true;
                self.recover = None;
            }
        }
        match &event.payload {
            Payload::SessionStart(_) => {
                self.failed_in.clear();
                self.session = Some(SessionView {
                    start: event.time,
                    activity: None,
                });
            }
            Payload::ConfigLoaded(_) => {
                self.failed_in.clear();
                self.configs += 1;
                self.stopped = false;
            }
            Payload::CheckingCycle(cycle) if cycle.event == togi_journal::CycleEvent::Start => {
                self.cycle_steps
                    .insert(cycle.cycle.get(), cycle.steps.clone());
            }
            Payload::CheckingCycle(cycle)
                if cycle.event == togi_journal::CycleEvent::End && cycle.passed && cycle.full =>
            {
                self.cycle_ends
                    .insert(event.seq, (cycle.cycle.get(), event.time));
            }
            Payload::SmuIntent(_) => self.restored = false,
            Payload::SmuReadback(readback) => {
                self.applied.insert(readback.core, readback.offset);
            }
            Payload::ProfileRestored(restored) => {
                self.restored = true;
                for (core, offset) in restored.offsets.iter() {
                    self.applied.insert(core, offset.observed());
                }
            }
            Payload::TrialIntent(intent) => {
                self.current = Some(Current {
                    intent: intent.clone(),
                    boot: event.boot.clone(),
                    boot_ended: false,
                });
                self.intents.insert(intent.trial.clone(), intent.clone());
                let operation = Operation::of(intent);
                self.failed_in.retain(|_, failed| *failed == operation);
                self.operation = Some(operation);
                self.restored = false;
                self.set_activity(intent.activity);
            }
            Payload::TrialStart(start) => {
                self.recover = None;
                self.starts.insert(start.trial.clone(), event.time);
            }
            Payload::TrialEnd(end) => self.trial_end(event, end, tuner),
            Payload::CorePhase(transition) => {
                self.tuned.insert(transition.core, transition.offset);
                self.queued.remove(&transition.core);
                let source = self.cited_source(event);
                self.record_failure_point(
                    transition.core,
                    transition.failure_point,
                    event.seq,
                    source,
                );
            }
            Payload::TunerDecision(decision) => {
                self.tuned.insert(decision.core, decision.to_offset);
                self.set_activity(Some(decision.activity));
                if decision.decision == togi_journal::Decision::Backoff {
                    self.backs
                        .insert(decision.core, self.sources.get(&event.seq).copied());
                }
                let source = match decision.activity {
                    Activity::Hunt => FailureSource::Named,
                    Activity::Search => FailureSource::Alone,
                    Activity::Checking | Activity::Deepening => self.cited_source(event),
                };
                self.record_failure_point(decision.core, decision.failure_point, event.seq, source);
                self.evidence.entry(decision.core).or_default().decision = Some(OffsetDecision {
                    seq: event.seq,
                    decision: decision.decision,
                    activity: decision.activity,
                    from: decision.from_offset,
                    to: decision.to_offset,
                });
            }
            Payload::ProfileChange(change) => {
                for (core, offset) in change.to.iter() {
                    self.tuned.insert(core, offset);
                }
            }
            Payload::Failure(failure) => {
                self.failures += 1;
                let carried = failure
                    .known_failure
                    .is_some_and(|known| self.carried.contains(&known));
                self.by_failure.insert(
                    event.seq,
                    FailureFacts {
                        at: Some(event.time),
                        signal: Some(failure.signal),
                        core: failure.core,
                        known: failure.known_failure.is_some(),
                        carried,
                        regime: failure.regime,
                        source: failure_source(failure),
                    },
                );
                if failure.known_failure.is_none() {
                    self.last_failure = Some(event.time);
                }
            }
            Payload::CrashDetected(crash) => {
                self.crashes += 1;
                let mut recover = RecoverFacts {
                    crash_at: event.time,
                    reset: crash.reset_reason,
                    intent: None,
                    end: None,
                };
                if let Some(current) = &self.current {
                    if current.boot == crash.previous_boot {
                        recover.intent = Some(current.intent.clone());
                        self.current = None;
                    }
                }
                self.recover = Some(recover);
            }
            Payload::TrialCarried(carried) => {
                self.carried.insert(event.seq);
                self.by_failure.insert(
                    event.seq,
                    FailureFacts {
                        at: None,
                        signal: carried.signal,
                        core: None,
                        known: true,
                        carried: true,
                        regime: Some(carried.class.regime),
                        source: FailureSource::Unrecorded,
                    },
                );
            }
            Payload::FailureCarried(carried) => {
                self.by_failure.insert(
                    event.seq,
                    FailureFacts {
                        at: None,
                        signal: Some(carried.failure.signal),
                        core: carried.failure.core,
                        known: true,
                        carried: true,
                        regime: None,
                        source: failure_source(&carried.failure),
                    },
                );
            }
            Payload::HuntStart(start) => self.hunt_start(event, start),
            Payload::HuntGroup(group) => {
                if group.probe.is_some() {
                    self.probes.insert(group.hunt.get());
                }
            }
            Payload::Combination(combination) => {
                let id = CombinationId::new(combination.combination);
                self.combination_seqs.insert(event.seq, id);
                self.combos.insert(
                    id,
                    ComboFacts {
                        hunt: combination.hunt.get(),
                        fallback: combination.fallback,
                    },
                );
            }
            Payload::DeadEnd(dead_end) => {
                if let (DeadEndCondition::FailureAtZero, Some(core)) =
                    (dead_end.condition, dead_end.core)
                {
                    self.record_failure_point(
                        core,
                        Some(TargetOffset::ZERO),
                        event.seq,
                        FailureSource::Named,
                    );
                }
                self.dead_end = Some(DeadEndView {
                    at: event.time,
                    condition: dead_end.condition,
                    detail: dead_end.detail.clone(),
                });
            }
            Payload::Shutdown(shutdown) => {
                self.stopped = true;
                self.stop = Some(StopView {
                    at: event.time,
                    reason: shutdown.reason,
                    saved: self.restored,
                });
            }
            Payload::CommandReset(reset) => {
                if let ResetTarget::Core(core) = reset.target {
                    self.queued.insert(core, event.seq);
                }
            }
            Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuWrite(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::Mce(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => {}
        }
        if let Some(line) = self.describe(event, journal) {
            self.history.push(line);
        }
        let table = words(event.kind());
        if table.listed {
            self.log_total += 1;
            #[cfg(test)]
            crate::corpus::journal_message(&tag_word(&event.payload), &event.msg);
            let msg = self.log_message(event);
            self.log.push(LogEntry {
                seq: event.seq,
                at: event.time,
                tag: tag_word(&event.payload),
                msg,
            });
            if self.log.len() >= 2 * LOG_LIMIT {
                self.log.drain(..self.log.len() - LOG_LIMIT);
            }
        }
    }

    /// The log's message for `event`: the recorded one, except that a profile event names only the cores whose
    /// offset it moved from the profile before it. The journal keeps the whole list for `togi events`.
    fn log_message(&mut self, event: &Event) -> String {
        let profile = match &event.payload {
            Payload::ProfileApplied(applied) => &applied.offsets,
            Payload::ProfileChange(change) => &change.to,
            Payload::ProfileRestored(restored) => &restored.offsets,
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
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => return event.msg.clone(),
        };
        let next: BTreeMap<CoreId, TargetOffset> = profile.iter().collect();
        let before = std::mem::replace(&mut self.logged_profile, next);
        profile_diff(&event.msg, &before, &self.logged_profile)
    }

    /// The source of the failure `event` cites, when it cites one the journal recorded.
    fn cited_source(&self, event: &Event) -> FailureSource {
        event
            .cause
            .iter()
            .find_map(|cause| self.by_failure.get(cause))
            .map_or(FailureSource::Unrecorded, |failure| failure.source)
    }

    /// Folds the failure constraint `event` leaves on `core`: a new offset records its source and event, the
    /// same offset keeps the evidence it has, none clears it.
    fn record_failure_point(
        &mut self,
        core: CoreId,
        point: Option<TargetOffset>,
        seq: Seq,
        source: FailureSource,
    ) {
        let evidence = self.evidence.entry(core).or_default();
        match point {
            None => {
                evidence.failure = None;
                self.failed_in.remove(&core);
            }
            Some(offset) if evidence.failure.is_none_or(|known| known.offset != offset) => {
                evidence.failure = Some(FailureEvidence {
                    offset,
                    source,
                    seq,
                });
                if let Some(operation) = &self.operation {
                    self.failed_in.insert(core, operation.clone());
                }
            }
            Some(_) => {}
        }
    }

    fn set_activity(&mut self, activity: Option<togi_journal::Activity>) {
        if let (Some(session), Some(activity)) = (&mut self.session, activity) {
            session.activity = Some(activity);
        }
    }

    fn clears_combination(&mut self, event: &Event) {
        let Payload::TunerDecision(decision) = &event.payload else {
            return;
        };
        if decision.decision != togi_journal::Decision::Backoff {
            return;
        }
        if let Some(id) = event
            .cause
            .iter()
            .find_map(|cause| self.combination_seqs.get(cause))
        {
            self.sources.insert(event.seq, *id);
        }
    }

    fn trial_end(&mut self, event: &Event, end: &TrialEnd, tuner: &TunerState) {
        let intent = self.intents.get(&end.trial);
        let view = TrialEndView {
            id: end.trial.clone(),
            at: event.time,
            regime: intent.map(|intent| intent.regime),
            cores: intent.map(trial_cores).unwrap_or_default(),
            outcome: end.outcome,
            signal: end.signal,
            core: end.core,
            duration: secs(end.duration_s),
            planned: intent.map(|intent| secs(intent.duration_s)),
            last_sample: end.last_sample_s.map(secs),
            stalled: end.stalled_core,
            tctl_max_c: end.tctl_max_c,
            voltage_v: end.voltage_request_median_v,
        };
        if let (Some(intent), Some(signal)) = (intent, end.signal) {
            if let (Some(hunt), Some(group), Outcome::Failure) =
                (intent.hunt, intent.group, end.outcome)
            {
                self.group_signals.insert((hunt.get(), group.get()), signal);
            }
        }
        if let Some(history) = tuner.trial_history() {
            if end.outcome == Outcome::Failure {
                self.failed.insert(end.trial.clone(), history);
            }
            self.counts.insert(
                end.trial.clone(),
                Count {
                    passed: history.passed,
                    index: history.trial,
                    of: history.needed,
                },
            );
            self.steps.insert(
                end.trial.clone(),
                history.step.map_or(0, |step| step.get().get()),
            );
        }
        self.ends.insert(end.trial.clone(), view.clone());
        if let Some(recover) = &mut self.recover {
            if recover
                .intent
                .as_ref()
                .is_some_and(|intent| intent.trial == end.trial)
            {
                recover.end = Some(view.clone());
            }
        }
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.intent.trial == end.trial)
        {
            self.current = None;
        }
        self.last = Some(view);
    }

    fn hunt_start(&mut self, event: &Event, start: &HuntStart) {
        let hunt = start.hunt.get();
        self.shapes.insert(
            hunt,
            HuntShape {
                failing: start.failing,
                parked: start.parked,
            },
        );
        self.hunt_started.insert(hunt, event.time);
        if let Some(intent) = start
            .trial
            .as_ref()
            .and_then(|trial| self.intents.get(trial))
        {
            if let Some(cycle) = intent.cycle {
                let step = start
                    .trial
                    .as_ref()
                    .and_then(|trial| self.steps.get(trial))
                    .copied()
                    .unwrap_or(0);
                self.check_hunts
                    .entry((cycle.get(), step))
                    .or_default()
                    .push(hunt);
            }
        }
    }
}

/// `recorded`, a profile event's message, with its core list replaced by the cores `now` moves from `before`
/// (`03 -15 → -14`); a message without a list, or the first profile there is, stays as recorded.
fn profile_diff(
    recorded: &str,
    before: &BTreeMap<CoreId, TargetOffset>,
    now: &BTreeMap<CoreId, TargetOffset>,
) -> String {
    let Some(list) = recorded.find(" [") else {
        return recorded.to_owned();
    };
    if before.is_empty() {
        return recorded.to_owned();
    }
    let moved: Vec<String> = now
        .iter()
        .filter(|(core, offset)| before.get(core) != Some(offset))
        .map(|(core, offset)| match before.get(core) {
            Some(was) => format!("{:02} {} → {}", core.get(), was.get(), offset.get()),
            None => format!("{:02} → {}", core.get(), offset.get()),
        })
        .collect();
    let head = recorded[..list].trim_end_matches(':');
    if moved.is_empty() {
        format!("{head}: no core's offset moved")
    } else {
        format!("{head}: {}", moved.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use togi_journal::ProfileChange;

    use super::profile_diff;
    use crate::tests::kit::{Script, Start, core, off};

    fn profile(
        offsets: &[(u8, i64)],
    ) -> std::collections::BTreeMap<togi_machine::CoreId, togi_machine::TargetOffset> {
        offsets
            .iter()
            .map(|&(id, value)| (core(id), off(value)))
            .collect()
    }

    #[test]
    fn a_profile_message_names_only_the_cores_that_moved() {
        let before = profile(&[(0, -43), (1, -33), (3, -15)]);
        let now = profile(&[(0, -43), (1, -33), (3, -14)]);
        let recorded = "profile applied: [00: -43, 01: -33, 03: -14]";
        assert_eq!(
            profile_diff(recorded, &before, &now),
            "profile applied: 03 -15 → -14"
        );
        assert_eq!(
            profile_diff(recorded, &now, &now),
            "profile applied: no core's offset moved"
        );
        assert_eq!(
            profile_diff(
                "profile changed [00: -43]",
                &profile(&[(1, -5)]),
                &profile(&[(0, -43), (1, -5)])
            ),
            "profile changed: 00 → -43"
        );
    }

    #[test]
    fn the_first_profile_and_a_message_without_a_list_stay_as_recorded() {
        let now = profile(&[(0, 0)]);
        let recorded = "profile applied: [00: 0]";
        assert_eq!(profile_diff(recorded, &profile(&[]), &now), recorded);
        let alone = "profile applied for alone trials: every core at CO 0";
        assert_eq!(profile_diff(alone, &now, &profile(&[(0, -1)])), alone);
    }

    #[test]
    fn the_journal_view_shows_a_profile_change_as_the_cores_it_moved() {
        let mut script = Script::new(&[(0, 0), (1, 0)], &[Start::search(0), Start::search(0)]);
        let change = |values: [i64; 2]| ProfileChange {
            from: None,
            to: [(core(0), off(values[0])), (core(1), off(values[1]))]
                .into_iter()
                .collect(),
        };
        script.append(change([-10, -10]).into(), &[]);
        script.append(change([-10, -12]).into(), &[]);
        let model = script.model();
        let log = model.snapshot().log();
        let last = log.entries.last().expect("a log entry");
        assert_eq!(last.tag, "profile change");
        assert!(last.msg.ends_with(": 01 -10 → -12"), "{}", last.msg);
    }
}
