//! What the fold remembers beside the journal's and the policy's states: the facts only the event stream says,
//! such as when a trial started, what the hardware read back, how the session stopped.
//! Per-core facts use dense optional slots; trial keys remain dynamic.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use jiff::Timestamp;
use togi_journal::{
    ApplyCause, DecisionId, Event, Payload, State as JournalState, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{BootId, CoreSet, ObservedOffset, PerCore};

use crate::history::{History, LOG_LIMIT, LogEntry};
use crate::model::{DeadEndView, SessionView, StopView, TrialEndView};
use crate::tuning::TuningClock;
use crate::words::{tag_word, words};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct Current {
    pub(crate) intent: TrialIntent,
    pub(crate) boot: BootId,
    pub(crate) boot_ended: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct RecoverFacts {
    pub(crate) crash_at: Timestamp,
    pub(crate) reset: Option<togi_machine::ResetKind>,
    pub(crate) intent: Option<TrialIntent>,
    pub(crate) end: Option<TrialEndView>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
enum RuntimeLifecycle {
    #[default]
    Running,
    Stopped {
        boot: BootId,
    },
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize, PartialEq, Clone)]
pub(crate) struct Facts {
    pub(crate) session: Option<SessionView>,
    pub(crate) intents: HashMap<TrialId, TrialIntent>,
    pub(crate) starts: HashMap<TrialId, Timestamp>,
    pub(crate) applied: PerCore<Option<ObservedOffset>>,
    pub(crate) current: Option<Current>,
    lifecycle: RuntimeLifecycle,
    pub(crate) restored: bool,
    pub(crate) configs: u32,
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
    #[serde(default)]
    pub(crate) day: togi_journal::DayQuestions,
    #[serde(default)]
    pub(crate) day_totals: crate::day_totals::DayTotals,
    /// The day steps a `day.applied` carried out: a proposal not listed here was never committed.
    #[serde(default)]
    pub(crate) applied_steps: BTreeSet<DecisionId>,
}

pub(crate) fn secs(seconds: u32) -> Duration {
    Duration::from_secs(u64::from(seconds))
}

/// The cores a trial loads: its one core, or its recorded set.
pub(crate) fn trial_cores(intent: &TrialIntent) -> CoreSet {
    match intent.core {
        Some(core) => std::iter::once(core).collect(),
        None => intent.cores,
    }
}

impl Facts {
    /// Takes one event, after the journal's state took it.
    pub(crate) fn fold(&mut self, event: &Event, journal: &JournalState) {
        self.tuning.fold(event);
        self.day.apply(event);
        self.day_totals.apply(event);
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
            Payload::DayApplied(applied) => {
                if let ApplyCause::Step { decision } = applied.cause {
                    self.applied_steps.insert(decision);
                }
            }
            Payload::SessionStart(_) => {
                self.session = Some(SessionView {
                    start: event.time,
                    activity: None,
                });
            }
            Payload::ConfigLoaded(_) => {
                self.configs += 1;
                self.lifecycle = RuntimeLifecycle::Running;
            }
            Payload::DayBootStart(_) => {
                if let RuntimeLifecycle::Stopped { boot } = &self.lifecycle
                    && *boot != event.boot
                {
                    self.lifecycle = RuntimeLifecycle::Running;
                }
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
            Payload::TrialEnd(end) => self.trial_end(event, end),
            Payload::Failure(failure) => {
                self.failures += 1;
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
            Payload::DeadEnd(dead_end) => {
                self.dead_end = Some(DeadEndView {
                    at: event.time,
                    condition: dead_end.condition,
                    detail: dead_end.detail.clone(),
                });
            }
            Payload::Shutdown(shutdown) => {
                self.lifecycle = RuntimeLifecycle::Stopped {
                    boot: event.boot.clone(),
                };
                self.stop = Some(StopView {
                    at: event.time,
                    reason: shutdown.reason,
                    saved: self.restored,
                });
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
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::TaskStart(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::JournalTorn(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::SessionImported(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayPause(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::CheckingCycle(_)
            | Payload::CorePhase(_)
            | Payload::TunerDecision(_)
            | Payload::ProfileChange(_)
            | Payload::TrialCarried(_)
            | Payload::FailureCarried(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::Combination(_)
            | Payload::CommandReset(_)
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

    pub(crate) const fn is_stopped(&self) -> bool {
        match self.lifecycle {
            RuntimeLifecycle::Running => false,
            RuntimeLifecycle::Stopped { .. } => true,
        }
    }

    fn set_activity(&mut self, activity: Option<togi_journal::Activity>) {
        if let (Some(session), Some(activity)) = (&mut self.session, activity) {
            session.activity = Some(activity);
        }
    }

    fn trial_end(&mut self, event: &Event, end: &TrialEnd) {
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
}
