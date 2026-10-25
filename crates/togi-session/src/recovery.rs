//! The run's fold over journal events: what a later boot needs to tell a crash from a clean stop, which trial is
//! open, which machine checks were already recorded and claimed, and which dead-end evidence stands.

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use jiff::Timestamp;
use togi_config::DeadEndThreshold;
use togi_journal::{
    Build, CrashDetected, DeadEnd, DeadEndCondition, Event, EventKind, NightId, Outcome, Payload,
    RetryTarget, Seq, ShutdownReason, SmuCommand, TaskId, TaskStart, TrialId, TrialIntent,
};
use togi_machine::{
    Backend, BiosContext, BootId, CcdId, Condition, CpuId, KernelCursor, ObservedOffset, PerCore,
    ResetReason, TargetOffset,
};
use togi_tuner::{CrashKind, recorded_kind};

use crate::error::FoldError;
use crate::evidence::{self, RetryDue};

/// An observation that stands as dead-end evidence until a dead end of its condition is recorded.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Cited {
    pub seq: Seq,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum IntentKind {
    Smu,
    Trial(TrialId),
}

/// An intent that no write, error or end has answered yet.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct OpenIntent {
    pub seq: Seq,
    pub boot: BootId,
    pub kind: IntentKind,
}

/// A trial's start and the last time anything of it was recorded.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Started {
    pub seq: Seq,
    pub cpus: Vec<CpuId>,
    pub time: Timestamp,
    pub mono: Option<Duration>,
    pub last_time: Timestamp,
    pub last_mono: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<togi_journal::ControllerEvidence>,
}

/// How far the write of the target a trial alone sets before its start got.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum TargetWrite {
    /// The intent to write the target, citing the trial's intent.
    Intended(Seq),
    /// The write that intent asked for, whose readback starts the trial's window.
    Written(Seq),
}

/// The trial that has an intent and no end.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct OpenTrial {
    pub seq: Seq,
    pub intent: TrialIntent,
    pub boot: BootId,
    pub started: Option<Started>,
    pub failure: Option<evidence::FailureSignal>,
    pub mces: Vec<Seq>,
    pub corrected: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faults: Vec<evidence::RecordedProcessFault>,
    /// The monotonic time from which machine checks count as the trial's: its intent, then the start the backend
    /// reported, then the readback of its target.
    pub window_start: Option<Duration>,
    pub target: Option<TargetWrite>,
    pub kernel_error: Option<String>,
}

impl OpenTrial {
    fn new(event: &Event, intent: TrialIntent) -> Self {
        Self {
            seq: event.seq,
            intent,
            boot: event.boot.clone(),
            started: None,
            failure: None,
            mces: Vec::new(),
            corrected: false,
            faults: Vec::new(),
            window_start: event.mono,
            target: None,
            kernel_error: None,
        }
    }

    pub(crate) fn failure(&self) -> Option<evidence::FailureSignal> {
        match self
            .started
            .as_ref()
            .and_then(|start| start.controller.as_ref())
        {
            Some(controller) => evidence::process_faults_failure(
                self.failure,
                &self.faults,
                &controller.tested_cpus,
            ),
            None => self.failure,
        }
    }

    /// How long the trial is known to have run: from its start to the last thing recorded of it. Nothing is
    /// recorded between events, so after a crash this is a lower bound.
    pub(crate) fn ran(&self) -> Result<Duration, jiff::Error> {
        let Some(started) = &self.started else {
            return Ok(Duration::ZERO);
        };
        match (started.mono, started.last_mono) {
            (Some(start), Some(last)) if last >= start => Ok(last.saturating_sub(start)),
            _ => Duration::try_from(started.last_time.duration_since(started.time)),
        }
    }

    pub(crate) fn includes_mce(&self, mono: Option<Duration>) -> bool {
        let since = self
            .window_start
            .or(self.started.as_ref().and_then(|started| started.mono))
            .unwrap_or(Duration::ZERO);
        evidence::mce_in_window(mono, since)
    }
}

/// A machine check recorded from another boot, for a crash to claim.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecoveredMce {
    pub seq: Seq,
    pub from_boot: BootId,
    pub corrected: bool,
    pub mono: Option<Duration>,
    /// The boot whose `crash.detected` first cited this machine check; it is no evidence of any other crash.
    pub claimed: Option<BootId>,
}

/// A machine check is the same one when its boot, monotonic time, CPU and bank are.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
struct MceKey {
    boot: BootId,
    mono_ns: Option<Duration>,
    cpu: CpuId,
    bank: u16,
}

impl MceKey {
    fn of(mce: &togi_machine::Mce, boot: &BootId) -> Self {
        Self {
            boot: boot.clone(),
            mono_ns: Some(mce.monotonic),
            cpu: mce.cpu,
            bank: mce.bank,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct BootRecord {
    last_kind: EventKind,
    applied: Option<(Seq, Condition)>,
    applied_mono: Option<Duration>,
    applying: bool,
}

impl BootRecord {
    const fn new(kind: EventKind) -> Self {
        Self {
            last_kind: kind,
            applied: None,
            applied_mono: None,
            applying: false,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
struct BackendState {
    retry: Option<(u8, u32)>,
    followed: bool,
    last_reason: Option<String>,
    streak: Vec<Seq>,
    /// The event that found the backend missing this night: its failed `backends.<backend>` preflight check, or
    /// a trial that found its program missing. It excludes the backend at once, without retries.
    #[serde(default)]
    missing: Option<Seq>,
}

/// The latest dead end and what has followed it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PendingDeadEnd {
    pub seq: Seq,
    pub dead_end: DeadEnd,
    /// The events the dead end cites.
    pub causes: Vec<Seq>,
    saved_entry: Option<bool>,
    shutdown: bool,
}

impl PendingDeadEnd {
    /// Whether the saved boot entry still has to be cleared.
    #[cfg(test)]
    pub(crate) const fn needs_saved_entry(&self) -> bool {
        self.saved_entry.is_none()
    }

    /// Whether the latest attempt to clear the saved boot entry succeeded; `None` before any.
    pub(crate) const fn saved_entry_cleared(&self) -> Option<bool> {
        self.saved_entry
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecoveryState {
    started: bool,
    build: Option<Build>,
    context: Option<BiosContext>,
    topology: PerCore<Option<CcdId>>,
    baseline: Option<(Seq, PerCore<ObservedOffset>)>,
    boots: Vec<BootId>,
    records: BTreeMap<BootId, BootRecord>,
    crashes: BTreeMap<BootId, (Seq, CrashDetected)>,
    backends: BTreeMap<Backend, BackendState>,
    kernel_retries: Vec<Seq>,
    kernel_cursors: BTreeMap<BootId, KernelCursor>,
    hunt_group: bool,
    unmatched: Vec<OpenIntent>,
    open: Option<OpenTrial>,
    #[serde(serialize_with = "togi_machine::serde_pairs::serialize_set")]
    mce_keys: HashSet<MceKey>,
    recovered: Vec<RecoveredMce>,
    smu: Option<Cited>,
    containment: Option<Cited>,
    thermal: Option<Cited>,
    stray: Vec<Seq>,
    trials: u32,
    dead_end: Option<PendingDeadEnd>,
    /// The boot the open night's first plan was made in: a day boot in another boot closes that night.
    #[serde(default)]
    night_boot: Option<BootId>,
    /// The night task whose start is recorded but whose trial is not, with that start: a restart that resumes the
    /// task keeps it.
    #[serde(default)]
    task: Option<(TaskStart, Timestamp)>,
}

impl RecoveryState {
    #[cfg(test)]
    pub(crate) fn replay<'a>(
        events: impl IntoIterator<Item = &'a Event>,
    ) -> Result<Self, FoldError> {
        let mut state = Self::default();
        for event in events {
            state.apply(event)?;
        }
        Ok(state)
    }

    /// Folds one event. An event the state cannot take in leaves it unchanged.
    pub(crate) fn apply(&mut self, event: &Event) -> Result<(), FoldError> {
        match (&event.payload, self.started) {
            (Payload::SessionStart(_), true) => {
                return Err(FoldError::DuplicateStart { seq: event.seq });
            }
            (Payload::SessionStart(_), false) => {}
            (_, false) => {
                return Err(FoldError::BeforeStart {
                    seq: event.seq,
                    kind: event.kind(),
                });
            }
            (_, true) => {}
        }
        self.follow_dead_end(event);
        self.track_boot(event);
        let armed = self.hunt_group;
        let seq = event.seq;
        match &event.payload {
            Payload::SessionStart(start) => {
                self.started = true;
                self.build.get_or_insert_with(|| start.build.clone());
                self.topology = start
                    .cores
                    .iter()
                    .map(|info| (info.core, Some(info.ccd)))
                    .collect();
            }
            Payload::ConfigLoaded(loaded) => {
                if let Some(build) = &mut self.build {
                    build.version.clone_from(&loaded.build.version);
                    build.rev.clone_from(&loaded.build.rev);
                    build.fixes = loaded.build.fixes;
                }
                self.kernel_boundary(
                    event,
                    loaded.kernel_cursor.as_ref(),
                    loaded.kernel_error.as_deref(),
                );
            }
            Payload::Shutdown(shutdown) => {
                self.kernel_boundary(
                    event,
                    shutdown.kernel_cursor.as_ref(),
                    shutdown.kernel_error.as_deref(),
                );
                if matches!(
                    shutdown.reason,
                    ShutdownReason::Signal | ShutdownReason::DeadEnd
                ) {
                    self.close_night();
                }
            }
            Payload::SessionContext(context) => self.context = Some(context.bios_context()),
            Payload::SessionBaseline(baseline) => {
                self.baseline = Some((seq, baseline.offsets));
            }
            Payload::HuntGroup(_) => self.hunt_group = true,
            Payload::HuntEnd(_) | Payload::ProfileChange(_) => self.hunt_group = false,
            Payload::ProfileApplied(applied) => {
                self.hunt_group = self.hunt_group && applied.condition == Condition::Parked;
                let record = self.record(event);
                record.applied = Some((seq, applied.condition));
                if !record.applying {
                    record.applied_mono = event.mono;
                }
                record.applying = false;
                self.stray.clear();
            }
            Payload::ProfileRestored(_) => {
                let record = self.record(event);
                record.applied = None;
                record.applying = false;
            }
            Payload::SmuIntent(intent) => self.smu_intent(event, intent.command(), armed),
            Payload::SmuWrite(_) => {
                self.drop_smu_intent();
                if let Some(open) = &mut self.open
                    && open.boot == event.boot
                    && let Some(TargetWrite::Intended(target)) = open.target
                    && event.cause.contains(&target)
                {
                    open.target = Some(TargetWrite::Written(seq));
                }
            }
            Payload::SmuError(error) => {
                self.drop_smu_intent();
                self.smu = Some(Cited {
                    seq,
                    detail: format!("SMU command failed: {}", error.error),
                });
            }
            Payload::SmuReadback(readback) => self.smu_readback(event, readback),
            Payload::TrialIntent(intent) => {
                self.hunt_group = self.hunt_group && intent.condition == Condition::Parked;
                self.task = None;
                self.unmatched.push(OpenIntent {
                    seq,
                    boot: event.boot.clone(),
                    kind: IntentKind::Trial(intent.trial.clone()),
                });
                self.trials += 1;
                self.open = Some(OpenTrial::new(event, intent.clone()));
                self.kernel_boundary(
                    event,
                    intent.kernel_cursor.as_ref(),
                    intent.kernel_error.as_deref(),
                );
                self.backend(intent.workload.backend()).followed = true;
            }
            Payload::TrialStart(start) => {
                if let Some(open) = &mut self.open
                    && open.intent.trial == start.trial
                {
                    open.started = Some(Started {
                        seq,
                        cpus: start.cpus.clone(),
                        time: event.time,
                        mono: event.mono,
                        last_time: event.time,
                        last_mono: event.mono,
                        controller: start.controller.clone(),
                    });
                    if let Some(ns) = start.window_start_ns {
                        open.window_start = Some(Duration::from_nanos(ns));
                    }
                }
            }
            Payload::TrialProgress(progress) => {
                self.trial_activity(&progress.trial, event);
                if let Some(open) = &mut self.open
                    && open.intent.trial == progress.trial
                {
                    if let Some(fault) = &progress.fault {
                        open.faults
                            .push(evidence::RecordedProcessFault::new(seq, fault));
                    } else if let Some(signal) = progress.signal {
                        open.failure = Some(evidence::FailureSignal {
                            signal,
                            core: progress.core,
                            pid: progress.pid,
                        });
                    }
                }
            }
            Payload::TrialSignal(signal) => self.trial_activity(&signal.trial, event),
            Payload::TrialSample(sample) => self.trial_activity(&sample.trial, event),
            Payload::Mce(mce) => self.record_mce(event, mce),
            Payload::CrashDetected(detected) => self.crash_detected(event, detected),
            Payload::TrialEnd(end) => {
                self.kernel_boundary(
                    event,
                    end.kernel_cursor.as_ref(),
                    end.kernel_error.as_deref(),
                );
                self.unmatched.retain(
                    |open| !matches!(&open.kind, IntentKind::Trial(trial) if *trial == end.trial),
                );
                if self
                    .open
                    .as_ref()
                    .is_some_and(|open| open.intent.trial == end.trial)
                {
                    self.trial_ended(event, end);
                    self.open = None;
                }
            }
            Payload::BackendRetry(retry) => match retry.backend {
                RetryTarget::KernelLog => self.kernel_retries.push(seq),
                RetryTarget::Mprime => self.backend_retried(Backend::Mprime, retry),
                RetryTarget::Ycruncher => self.backend_retried(Backend::Ycruncher, retry),
            },
            Payload::DeadEnd(dead_end) => self.dead_ended(seq, &event.cause, dead_end),
            Payload::DayBootStart(_) => {
                if self
                    .night_boot
                    .as_ref()
                    .is_some_and(|boot| *boot != event.boot)
                {
                    self.close_night();
                }
            }
            Payload::NightPlan(plan) => {
                if plan.night.is_none() {
                    self.night_boot = Some(event.boot.clone());
                }
            }
            Payload::TaskStart(start) => self.task = Some((*start, event.time)),
            Payload::PreflightCheck(check) => {
                if !check.ok
                    && let Some(backend) =
                        check
                            .check
                            .strip_prefix(BACKEND_CHECK_PREFIX)
                            .and_then(|name| {
                                [Backend::Mprime, Backend::Ycruncher]
                                    .into_iter()
                                    .find(|backend| backend.as_str() == name)
                            })
                {
                    self.backend(backend).missing.get_or_insert(seq);
                }
            }
            Payload::SessionWarning(_)
            | Payload::SessionNotice(_)
            | Payload::Failure(_)
            | Payload::CorePhase(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::TrialCarried(_)
            | Payload::FailureCarried(_)
            | Payload::TunerDecision(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::JournalTorn(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::SessionImported(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayApplied(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
        Ok(())
    }

    fn follow_dead_end(&mut self, event: &Event) {
        let Some(pending) = &mut self.dead_end else {
            return;
        };
        match &event.payload {
            Payload::BootSavedEntry(entry) if event.cause.contains(&pending.seq) => {
                pending.saved_entry = Some(entry.error.is_none());
            }
            Payload::Shutdown(shutdown) => {
                pending.shutdown |= shutdown.reason == ShutdownReason::DeadEnd;
            }
            Payload::BootSavedEntry(_)
            | Payload::SessionStart(_)
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
            | Payload::TrialIntent(_)
            | Payload::TaskStart(_)
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
            | Payload::BootLeaveReason(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
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
            | Payload::DayApplied(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
    }

    /// Admits the event's boot as a tuning boot and tracks its last kind. Day events and a repaired tear record
    /// no tuning activity: a boot with only those is never a crash, and cannot make a later tuning start in it
    /// look like a same-boot restart.
    fn track_boot(&mut self, event: &Event) {
        if event.kind().is_day() || event.kind() == EventKind::JournalTorn {
            return;
        }
        if let Some(record) = self.records.get_mut(&event.boot) {
            if !matches!(
                event.kind(),
                EventKind::SessionWarning | EventKind::BootLeaveReason
            ) {
                record.last_kind = event.kind();
            }
        } else {
            self.boots.push(event.boot.clone());
            self.records
                .insert(event.boot.clone(), BootRecord::new(event.kind()));
        }
    }

    fn record(&mut self, event: &Event) -> &mut BootRecord {
        self.records
            .entry(event.boot.clone())
            .or_insert_with(|| BootRecord::new(event.kind()))
    }

    fn backend(&mut self, backend: Backend) -> &mut BackendState {
        self.backends.entry(backend).or_default()
    }

    fn drop_smu_intent(&mut self) {
        self.unmatched
            .retain(|open| !matches!(open.kind, IntentKind::Smu));
    }

    fn smu_intent(&mut self, event: &Event, command: SmuCommand, armed: bool) {
        self.drop_smu_intent();
        if let Some(open) = &mut self.open
            && open.boot == event.boot
            && open.started.is_none()
            && event.cause.contains(&open.seq)
            && command.core().is_some()
            && command.core() == open.intent.core
            && open.intent.offset == Some(command.offset())
        {
            open.target = Some(TargetWrite::Intended(event.seq));
        }
        self.unmatched.push(OpenIntent {
            seq: event.seq,
            boot: event.boot.clone(),
            kind: IntentKind::Smu,
        });
        let baseline_seq = self.baseline.as_ref().map(|(seq, _)| *seq);
        let open_trial = self.open.is_some();
        let record = self.record(event);
        if command.offset().get() != 0
            && !open_trial
            && !baseline_seq.is_some_and(|seq| event.cause.contains(&seq))
        {
            record.applied = Some((
                event.seq,
                if armed {
                    Condition::Parked
                } else {
                    Condition::Together
                },
            ));
            if !record.applying {
                record.applied_mono = event.mono;
                record.applying = true;
            }
            self.stray.clear();
        }
    }

    fn smu_readback(&mut self, event: &Event, readback: &togi_journal::SmuReadback) {
        if let Some(expected) = readback.expected
            && expected.observed() != readback.offset
        {
            self.smu = Some(Cited {
                seq: event.seq,
                detail: format!(
                    "core {:02} reads CO {} after writing {}",
                    readback.core,
                    readback.offset.0,
                    expected.get()
                ),
            });
        }
        if let Some(open) = &mut self.open
            && open.boot == event.boot
            && open.started.is_none()
            && let Some(TargetWrite::Written(write)) = open.target
            && event.cause.contains(&write)
            && open.intent.core == Some(readback.core)
            && readback.expected.is_some()
            && readback.expected == open.intent.offset
            && readback.expected.map(TargetOffset::observed) == Some(readback.offset)
        {
            open.window_start = event.mono;
        }
    }

    fn trial_activity(&mut self, trial: &TrialId, event: &Event) {
        if let Some(open) = &mut self.open
            && open.intent.trial == *trial
            && let Some(started) = &mut open.started
        {
            started.last_time = event.time;
            started.last_mono = event.mono;
        }
    }

    fn kernel_boundary(
        &mut self,
        event: &Event,
        cursor: Option<&KernelCursor>,
        error: Option<&str>,
    ) {
        if let Some(cursor) = cursor {
            self.kernel_cursors
                .insert(event.boot.clone(), cursor.clone());
        }
        if let Some(open) = &mut self.open
            && open.boot == event.boot
            && error.is_some()
        {
            open.kernel_error = evidence::join_diagnostic(open.kernel_error.as_deref(), error);
        }
    }

    fn record_mce(&mut self, event: &Event, mce: &togi_journal::Mce) {
        let boot = mce.from_boot.as_ref().unwrap_or(&event.boot);
        self.mce_keys.insert(MceKey {
            boot: boot.clone(),
            mono_ns: mce.monotonic_ns.map(Duration::from_nanos),
            cpu: mce.cpu,
            bank: mce.bank,
        });
        let mono = mce.monotonic_ns.map(Duration::from_nanos);
        if mce.between_trials {
            return;
        }
        if let Some(from_boot) = &mce.from_boot {
            self.recovered.push(RecoveredMce {
                seq: event.seq,
                from_boot: from_boot.clone(),
                corrected: mce.corrected,
                mono,
                claimed: None,
            });
            return;
        }
        let Some(open) = &mut self.open else {
            return;
        };
        if mce
            .trial
            .as_ref()
            .is_some_and(|trial| *trial != open.intent.trial)
        {
            return;
        }
        if mono.is_some() && (*boot != open.boot || !open.includes_mce(mono)) {
            return;
        }
        open.mces.push(event.seq);
        open.corrected |= mce.corrected;
    }

    fn crash_detected(&mut self, event: &Event, detected: &CrashDetected) {
        self.kernel_retries.clear();
        self.crashes.insert(
            detected.previous_boot.clone(),
            (event.seq, detected.clone()),
        );
        let in_trial = self
            .open
            .as_ref()
            .is_some_and(|open| open.boot == detected.previous_boot);
        let reason = ResetReason {
            kind: detected.reset_reason,
            raw: detected.reset_reason_raw.clone(),
            supported: detected.reset_reason.is_some(),
        };
        let outcome = recorded_kind(detected.stray, detected.inconclusive, &reason, in_trial);
        if outcome == CrashKind::Thermal {
            self.thermal = Some(Cited {
                seq: event.seq,
                detail: format!(
                    "the machine reset on a thermal trip ({}); check cooling before tuning again",
                    detected.reset_reason_raw.join("; ")
                ),
            });
        }
        for recovered in &mut self.recovered {
            if recovered.claimed.is_none() && event.cause.contains(&recovered.seq) {
                recovered.claimed = Some(detected.previous_boot.clone());
            }
        }
        self.unmatched
            .retain(|open| open.boot != detected.previous_boot);
        match outcome {
            CrashKind::Stray => self.stray.push(event.seq),
            CrashKind::Idle | CrashKind::InTrial | CrashKind::Inconclusive | CrashKind::Thermal => {
            }
        }
    }

    fn trial_ended(&mut self, event: &Event, end: &togi_journal::TrialEnd) {
        let Some(open) = &self.open else {
            return;
        };
        let backend = open.intent.workload.backend();
        let start_cpus = open
            .started
            .as_ref()
            .map(|started| started.cpus.clone())
            .unwrap_or_default();
        match end.evidence_outcome() {
            Outcome::Inconclusive => {
                if !end.interrupted {
                    let state = self.backend(backend);
                    state.last_reason.clone_from(&end.reason);
                    state.streak.push(event.seq);
                }
            }
            Outcome::Pass | Outcome::Failure => {
                self.backend(backend).streak.clear();
            }
        }
        if let Some(detail) =
            containment_detail(&end.escaped, &start_cpus, end.containment_error.as_deref())
        {
            self.containment = Some(Cited {
                seq: event.seq,
                detail,
            });
        }
        if end.backend_missing {
            self.backend(backend).missing.get_or_insert(event.seq);
        }
    }

    fn backend_retried(&mut self, backend: Backend, retry: &togi_journal::BackendRetry) {
        let state = self.backend(backend);
        state.retry = Some((retry.attempt, retry.wait_s));
        state.followed = false;
    }

    /// A closing witness ends the night: its backend exclusions, retries and started task do not carry into the
    /// next one.
    fn close_night(&mut self) {
        self.backends.clear();
        self.night_boot = None;
        self.task = None;
    }

    fn dead_ended(&mut self, seq: Seq, causes: &[Seq], dead_end: &DeadEnd) {
        self.dead_end = Some(PendingDeadEnd {
            seq,
            dead_end: dead_end.clone(),
            causes: causes.to_vec(),
            saved_entry: None,
            shutdown: false,
        });
        match dead_end.condition {
            DeadEndCondition::Smu => self.smu = None,
            DeadEndCondition::Containment => self.containment = None,
            DeadEndCondition::NoEvidence => {
                for state in self.backends.values_mut() {
                    state.streak.clear();
                    state.missing = None;
                }
                self.kernel_retries.clear();
            }
            DeadEndCondition::ThermalTrip => self.thermal = None,
            DeadEndCondition::BootLoop => self.stray.clear(),
            DeadEndCondition::FailureAtZero
            | DeadEndCondition::Preflight
            | DeadEndCondition::Defect => {}
        }
    }
}

/// The prefix of a backend's own `preflight.check` name, `backends.<backend>`.
pub(crate) const BACKEND_CHECK_PREFIX: &str = "backends.";

/// Why a backend is excluded for the rest of the night.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Unavailable<'a> {
    /// The failed preflight check or trial that found it missing.
    Missing(&'a Seq),
    /// The trials of the streak that stayed inconclusive through every retry.
    Exhausted(&'a [Seq]),
}

impl Unavailable<'_> {
    pub(crate) fn causes(&self) -> &[Seq] {
        match self {
            Self::Missing(seq) => std::slice::from_ref(*seq),
            Self::Exhausted(streak) => streak,
        }
    }
}

impl std::fmt::Display for Unavailable<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(_) => f.write_str("missing"),
            Self::Exhausted(streak) => write!(f, "inconclusive {} times in a row", streak.len()),
        }
    }
}

pub(crate) fn containment_detail(
    escaped: &[CpuId],
    allowed: &[CpuId],
    uncontained: Option<&str>,
) -> Option<String> {
    uncontained.map(str::to_owned).or_else(|| {
        (!escaped.is_empty()).then(|| {
            format!(
                "backend thread on cpu {} outside allowed cpus {}",
                cpu_list(escaped),
                cpu_list(allowed)
            )
        })
    })
}

fn cpu_list(cpus: &[CpuId]) -> String {
    let mut list = String::new();
    for (i, cpu) in cpus.iter().enumerate() {
        if i > 0 {
            list.push(',');
        }
        list.push_str(&cpu.to_string());
    }
    list
}

/// Queries over the fold.
impl RecoveryState {
    /// The journal holds a `session.start`.
    pub(crate) const fn started(&self) -> bool {
        self.started
    }

    pub(crate) fn boots(&self) -> &[BootId] {
        &self.boots
    }

    /// The kind of the last event of the boot that is not a warning or a leave reason: those never replace a
    /// shutdown, or the next boot would invent a crash.
    pub(crate) fn last_kind(&self, boot: &BootId) -> Option<EventKind> {
        self.records.get(boot).map(|record| record.last_kind)
    }

    #[cfg(test)]
    pub(crate) fn build(&self) -> Option<&Build> {
        self.build.as_ref()
    }

    pub(crate) fn context(&self) -> Option<&BiosContext> {
        self.context.as_ref()
    }

    pub(crate) fn topology(&self) -> &PerCore<Option<CcdId>> {
        &self.topology
    }

    pub(crate) fn baseline(&self) -> Option<(Seq, &PerCore<ObservedOffset>)> {
        self.baseline.as_ref().map(|(seq, offsets)| (*seq, offsets))
    }

    /// Boots before `current` with no shutdown and no `crash.detected`, oldest first.
    pub(crate) fn crashed_boots<'a>(
        &'a self,
        current: &'a BootId,
    ) -> impl Iterator<Item = &'a BootId> + 'a {
        self.boots.iter().filter(move |boot| {
            *boot != current
                && self.last_kind(boot) != Some(EventKind::Shutdown)
                && !self.crashes.contains_key(*boot)
        })
    }

    /// The boot that followed `boot` in the journal, or `current` when none has.
    pub(crate) fn next_boot<'a>(&'a self, boot: &BootId, current: &'a BootId) -> &'a BootId {
        self.boots
            .iter()
            .position(|known| known == boot)
            .and_then(|at| self.boots.get(at + 1))
            .unwrap_or(current)
    }

    /// The machine checks a crash of `boot` may cite: those recorded from `boot` and not claimed by another crash,
    /// and the uncorrected ones its successor's kernel retained, which belong to the crash before it.
    pub(crate) fn recorded_for(&self, boot: &BootId, current: &BootId) -> Vec<Seq> {
        let next = self.next_boot(boot, current);
        self.recovered
            .iter()
            .filter(|mce| mce.claimed.as_ref().is_none_or(|claimed| claimed == boot))
            .filter(|mce| {
                !(mce.from_boot == *boot
                    && self
                        .open
                        .as_ref()
                        .is_some_and(|open| open.boot == *boot && !open.includes_mce(mce.mono)))
            })
            .filter(|mce| evidence::crash_owns(&mce.from_boot, mce.corrected, boot, next))
            .map(|mce| mce.seq)
            .collect()
    }

    /// The latest intent of `boot` that nothing answered, not the last event of the boot.
    pub(crate) fn latest_unmatched_intent(&self, boot: &BootId) -> Option<Seq> {
        self.unmatched
            .iter()
            .rev()
            .find(|open| open.boot == *boot)
            .map(|open| open.seq)
    }

    #[cfg(test)]
    pub(crate) fn unmatched_intents(&self) -> &[OpenIntent] {
        &self.unmatched
    }

    pub(crate) fn open_trial(&self) -> Option<&OpenTrial> {
        self.open.as_ref()
    }

    /// Whether the journal has a machine check with the identity of `mce` of `boot`.
    pub(crate) fn has_recorded(&self, boot: &BootId, mce: &togi_machine::Mce) -> bool {
        self.mce_keys.contains(&MceKey::of(mce, boot))
    }

    /// The latest dead end unless a dead-end shutdown followed it.
    pub(crate) fn pending_dead_end(&self) -> Option<&PendingDeadEnd> {
        self.dead_end.as_ref().filter(|pending| !pending.shutdown)
    }

    pub(crate) fn controller_rotation(&self) -> u64 {
        u64::from(self.trials)
    }

    pub(crate) fn next_trial_id(&self) -> TrialId {
        TrialId::from(format!("{:04}", self.trials + 1))
    }

    /// When `task` of `night` started, if its start is recorded and no trial of it followed yet.
    pub(crate) fn task_start(&self, night: NightId, task: TaskId) -> Option<Timestamp> {
        self.task
            .filter(|(start, _)| start.night == night && start.task == task)
            .map(|(_, time)| time)
    }

    pub(crate) fn kernel_cursor(&self, boot: &BootId) -> Option<&KernelCursor> {
        self.kernel_cursors.get(boot)
    }

    /// The profile application of `boot` that no restoration followed: its seq and condition.
    pub(crate) fn applied(&self, boot: &BootId) -> Option<(Seq, Condition)> {
        self.records.get(boot).and_then(|record| record.applied)
    }

    /// The monotonic time the profile application of `boot` first wrote at.
    #[cfg(test)]
    pub(crate) fn applied_mono(&self, boot: &BootId) -> Option<Duration> {
        self.records
            .get(boot)
            .and_then(|record| record.applied_mono)
    }

    /// Whether `boot` began applying a profile that `profile.applied` has not completed.
    #[cfg(test)]
    pub(crate) fn applying(&self, boot: &BootId) -> bool {
        self.records.get(boot).is_some_and(|record| record.applying)
    }

    #[cfg(test)]
    pub(crate) const fn hunt_group_armed(&self) -> bool {
        self.hunt_group
    }

    #[cfg(test)]
    pub(crate) fn crash_of(&self, boot: &BootId) -> Option<Seq> {
        self.crashes.get(boot).map(|(seq, _)| *seq)
    }

    /// The `crash.detected` that classified the crash of `boot`.
    pub(crate) fn crash(&self, boot: &BootId) -> Option<(Seq, &CrashDetected)> {
        self.crashes
            .get(boot)
            .map(|(seq, detected)| (*seq, detected))
    }

    pub(crate) fn stray(&self) -> &[Seq] {
        &self.stray
    }

    pub(crate) fn smu_evidence(&self) -> Option<&Cited> {
        self.smu.as_ref()
    }

    pub(crate) fn containment_evidence(&self) -> Option<&Cited> {
        self.containment.as_ref()
    }

    pub(crate) fn thermal_evidence(&self) -> Option<&Cited> {
        self.thermal.as_ref()
    }

    /// The seqs of the consecutive inconclusive trials of `backend`, an interrupted one excepted.
    #[cfg(test)]
    pub(crate) fn streak(&self, backend: Backend) -> &[Seq] {
        self.backends
            .get(&backend)
            .map_or(&[], |state| state.streak.as_slice())
    }

    pub(crate) fn last_inconclusive_reason(&self, backend: Backend) -> Option<&str> {
        self.backends
            .get(&backend)
            .and_then(|state| state.last_reason.as_deref())
    }

    /// Every backend excluded for the rest of the night, with the events that excluded it: the failed preflight
    /// check or trial that found it missing, or the trials of a streak that stayed inconclusive through every retry.
    pub(crate) fn unavailable_backends(
        &self,
        threshold: DeadEndThreshold,
    ) -> Vec<(Backend, Unavailable<'_>)> {
        self.backends
            .iter()
            .filter_map(|(backend, state)| {
                let unavailable = match &state.missing {
                    Some(seq) => Unavailable::Missing(seq),
                    None if evidence::backend_exhausted(state.streak.len(), threshold) => {
                        Unavailable::Exhausted(&state.streak)
                    }
                    None => return None,
                };
                Some((*backend, unavailable))
            })
            .collect()
    }

    /// The wait to sit out before the next trial of `backend`, and whether a `backend.retry` has to record it:
    /// not when that attempt's record is durable and no trial followed it.
    pub(crate) fn backend_retry_due(
        &self,
        backend: Backend,
        threshold: DeadEndThreshold,
    ) -> Option<RetryDue> {
        let state = self.backends.get(&backend);
        let streak = state.map_or(0, |state| state.streak.len());
        let mut due = evidence::backend_retry(streak, threshold)?;
        let recorded = state
            .filter(|state| !state.followed)
            .and_then(|state| state.retry)
            .is_some_and(|(attempt, _)| attempt == due.attempt);
        due.record = !recorded;
        Some(due)
    }

    /// The kernel-log retries recorded since the last crash was classified.
    pub(crate) fn kernel_retries(&self) -> &[Seq] {
        &self.kernel_retries
    }
}

#[cfg(test)]
pub(crate) mod tests;
