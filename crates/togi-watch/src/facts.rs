//! What the fold remembers beside the journal's and the tuner's states: the facts only the event stream says,
//! such as when a trial started, how a group failed, which backoff cleared which combination.
//! Per-core facts use dense optional slots; trial, cycle, group and combination keys remain dynamic.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::time::Duration;

use jiff::Timestamp;
use togi_journal::{
    Event, HuntStart, Outcome, Payload, ResetTarget, Seq, State as JournalState, TrialEnd, TrialId,
    TrialIntent,
};
use togi_machine::{
    BootId, CoreId, CoreSet, ObservedOffset, PerCore, Regime, Signal, TargetOffset,
};
use togi_tuner::{CombinationId, Load, State as TunerState, TrialHistory};

use crate::history::{History, LOG_LIMIT, LogEntry};
use crate::model::{Count, DeadEndView, HuntShape, SessionView, StopView, TrialEndView};
use crate::tuning::TuningClock;
use crate::words::{tag_word, words};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct Current {
    pub(crate) intent: TrialIntent,
    pub(crate) boot: BootId,
    pub(crate) boot_ended: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct FailureFacts {
    pub(crate) at: Option<Timestamp>,
    pub(crate) signal: Option<Signal>,
    pub(crate) core: Option<CoreId>,
    pub(crate) known: bool,
    pub(crate) carried: bool,
    pub(crate) regime: Option<Regime>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct HuntStartFacts {
    pub(crate) at: Timestamp,
    pub(crate) named: bool,
    pub(crate) live: u32,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct ComboFacts {
    pub(crate) hunt: u32,
    pub(crate) fallback: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct RecoverFacts {
    pub(crate) crash_at: Timestamp,
    pub(crate) reset: Option<togi_machine::ResetKind>,
    pub(crate) intent: Option<TrialIntent>,
    pub(crate) end: Option<TrialEndView>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Clone)]
pub(crate) struct Facts {
    pub(crate) session: Option<SessionView>,
    pub(crate) intents: HashMap<TrialId, TrialIntent>,
    pub(crate) starts: HashMap<TrialId, Timestamp>,
    pub(crate) ends: HashMap<TrialId, TrialEndView>,
    #[serde(with = "togi_machine::serde_pairs")]
    pub(crate) group_signals: HashMap<(u32, u32), Signal>,
    pub(crate) applied: PerCore<Option<ObservedOffset>>,
    pub(crate) tuned: PerCore<Option<TargetOffset>>,
    pub(crate) current: Option<Current>,
    pub(crate) steps: HashMap<TrialId, u32>,
    pub(crate) counts: HashMap<TrialId, Count>,
    pub(crate) failed: HashMap<TrialId, TrialHistory>,
    pub(crate) by_failure: HashMap<Seq, FailureFacts>,
    pub(crate) backs: PerCore<Option<CombinationId>>,
    pub(crate) sources: HashMap<Seq, CombinationId>,
    pub(crate) combination_seqs: HashMap<Seq, CombinationId>,
    pub(crate) combos: BTreeMap<CombinationId, ComboFacts>,
    #[serde(serialize_with = "togi_machine::serde_pairs::serialize_set")]
    pub(crate) probes: HashSet<u32>,
    pub(crate) stopped: bool,
    pub(crate) restored: bool,
    pub(crate) configs: u32,
    pub(crate) carrying: bool,
    #[serde(with = "togi_machine::serde_pairs")]
    pub(crate) check_hunts: HashMap<(u32, u32), Vec<u32>>,
    pub(crate) cycle_steps: HashMap<u32, Vec<Regime>>,
    pub(crate) hunt_starts: HashMap<u32, HuntStartFacts>,
    #[serde(with = "togi_machine::serde_pairs")]
    pub(crate) groups: HashMap<(u32, u32), togi_journal::HuntGroup>,
    pub(crate) carried: BTreeSet<Seq>,
    pub(crate) shapes: BTreeMap<u32, HuntShape>,
    pub(crate) hunt_started: BTreeMap<u32, Timestamp>,
    pub(crate) queued: PerCore<Option<Seq>>,
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
}

pub(crate) fn secs(seconds: u32) -> Duration {
    Duration::from_secs(u64::from(seconds))
}

pub(crate) fn trial_cores(intent: &TrialIntent) -> CoreSet {
    Load::recorded(intent.core, intent.cores).cores()
}

impl Facts {
    /// Takes one event, after both states took it. The tuner state has folded `event`, so a trial end finds its
    /// requirement in [`TunerState::trial_history`].
    pub(crate) fn fold(&mut self, event: &Event, journal: &JournalState, tuner: &TunerState) {
        self.tuning.fold(event);
        self.clears_combination(event);
        if let Some(current) = &mut self.current {
            if !event.kind().is_day()
                && !event.boot.as_str().is_empty()
                && !current.boot.as_str().is_empty()
                && event.boot != current.boot
            {
                current.boot_ended = true;
                self.recover = None;
            }
        }
        match &event.payload {
            Payload::SessionStart(_) => {
                self.session = Some(SessionView {
                    start: event.time,
                    activity: None,
                });
            }
            Payload::ConfigLoaded(_) => {
                self.configs += 1;
                self.stopped = false;
            }
            Payload::CheckingCycle(cycle) if cycle.event == togi_journal::CycleEvent::Start => {
                self.cycle_steps
                    .insert(cycle.cycle.get(), cycle.steps.clone());
            }
            Payload::SmuIntent(_) => self.restored = false,
            Payload::SmuReadback(readback) => {
                self.applied.set(readback.core, Some(readback.offset));
            }
            Payload::ProfileRestored(restored) => {
                self.restored = true;
                for (core, offset) in restored.offsets.iter() {
                    self.applied.set(core, Some(offset.observed()));
                }
            }
            Payload::TrialIntent(intent) => {
                self.current = Some(Current {
                    intent: intent.clone(),
                    boot: event.boot.clone(),
                    boot_ended: false,
                });
                self.intents.insert(intent.trial.clone(), intent.clone());
                self.restored = false;
                self.set_activity(intent.activity);
            }
            Payload::TrialStart(start) => {
                self.recover = None;
                self.starts.insert(start.trial.clone(), event.time);
            }
            Payload::TrialEnd(end) => self.trial_end(event, end, tuner),
            Payload::CorePhase(transition) => {
                self.tuned.set(transition.core, Some(transition.offset));
                self.queued.set(transition.core, None);
            }
            Payload::TunerDecision(decision) => {
                self.tuned.set(decision.core, Some(decision.to_offset));
                self.set_activity(Some(decision.activity));
                if decision.decision == togi_journal::Decision::Backoff {
                    self.backs
                        .set(decision.core, self.sources.get(&event.seq).copied());
                }
            }
            Payload::ProfileChange(change) => {
                for (core, offset) in change.to.iter() {
                    self.tuned.set(core, Some(offset));
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
                    self.queued.set(core, Some(event.seq));
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
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
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
            self.log.push(LogEntry {
                seq: event.seq,
                at: event.time,
                tag: tag_word(&event.payload),
                msg: event.msg.clone(),
            });
            if self.log.len() >= 2 * LOG_LIMIT {
                self.log.drain(..self.log.len() - LOG_LIMIT);
            }
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
