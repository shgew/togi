use std::num::NonZeroU32;

use jiff::{SignedDuration, Timestamp};
use togi_config::Config;
use togi_journal::{
    Activity, Attribution, Build, CheckingCycle, ConfigLoaded, CrashDetected, CycleEvent, Decision,
    Event, Failure, FixStamp, HuntEnd, HuntGroup, HuntOutcome, HuntStage, HuntStart, Outcome,
    Payload, SCHEMA, Seq, SessionStart, Shutdown, ShutdownReason, Stamp, TrialEnd, TrialId,
    TrialIntent, TrialStart, TunerDecision,
};
use togi_machine::{
    BootId, CcdId, Condition, CoreId, CoreInfo, CoreSet, PerCore, Regime, Signal, TargetOffset,
    WorkloadId,
};
use togi_tuner::RULESET;

use crate::{Journal, report};

pub(super) const SESSION: &str = "20260101T000000Z";
pub(super) const EPOCH: Timestamp = Timestamp::constant(1_767_225_600, 0);

pub(super) fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

pub(super) fn set(cores: &[i64]) -> CoreSet {
    cores.iter().map(|&n| core(n)).collect()
}

/// A profile of the offsets of cores 0, 1, ... in order.
pub(super) fn profile(offsets: &[i64]) -> PerCore<TargetOffset> {
    offsets
        .iter()
        .enumerate()
        .map(|(n, &offset)| {
            (
                core(i64::try_from(n).unwrap()),
                TargetOffset::new(offset).unwrap(),
            )
        })
        .collect()
}

pub(super) fn target(offset: i64) -> TargetOffset {
    TargetOffset::new(offset).unwrap()
}

pub(super) fn nz(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

pub(super) fn seq(n: u64) -> Seq {
    Seq::new(n).unwrap()
}

pub(super) fn workload(regime: Regime, index: usize) -> WorkloadId {
    WorkloadId::new(regime, index).unwrap()
}

pub(super) fn build(version: &str, rev: Option<&str>) -> Build {
    Build {
        version: version.to_owned(),
        rev: rev.map(str::to_owned),
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: 1,
    }
}

/// A journal under construction: `session.start` first, then events one second apart unless moved, in the
/// boot named by `boot`.
pub(super) struct Log {
    pub(super) events: Vec<Event>,
    boot: String,
    now: Timestamp,
    mono: Option<std::time::Duration>,
}

impl Log {
    pub(super) fn new(cores: &[(i64, u8)]) -> Self {
        Self::with_build(cores, build("test", Some("fixed")))
    }

    pub(super) fn with_build(cores: &[(i64, u8)], build: Build) -> Self {
        let mut log = Self {
            events: Vec::new(),
            boot: "a".to_owned(),
            now: EPOCH,
            mono: None,
        };
        log.add(SessionStart {
            build,
            session: SESSION.parse().unwrap(),
            cores: cores
                .iter()
                .map(|&(n, ccd)| CoreInfo {
                    core: core(n),
                    ccd: CcdId(ccd),
                    cpus: Vec::new(),
                })
                .collect(),
        });
        log
    }

    pub(super) fn boot(&mut self, name: &str) -> &mut Self {
        name.clone_into(&mut self.boot);
        self
    }

    /// The next event is at `seconds` after the epoch.
    pub(super) fn at(&mut self, seconds: i64) -> &mut Self {
        self.now = EPOCH
            .checked_add(SignedDuration::from_secs(seconds))
            .unwrap();
        self
    }

    pub(super) fn mono(&mut self, milliseconds: u64) -> &mut Self {
        self.mono = Some(std::time::Duration::from_millis(milliseconds));
        self
    }

    pub(super) fn add(&mut self, payload: impl Into<Payload>) -> Seq {
        self.add_caused(&[], payload)
    }

    /// An event citing `cause`, then one second on.
    pub(super) fn add_caused(&mut self, cause: &[Seq], payload: impl Into<Payload>) -> Seq {
        let number = Seq::new(self.events.len() as u64 + 1).unwrap();
        assert!(
            cause.iter().all(|&c| c < number),
            "every cause precedes the event it explains: {cause:?} for #{number}"
        );
        self.events.push(Event::new(
            number,
            Stamp {
                time: self.now,
                mono: self.mono,
            },
            BootId::from(self.boot.clone()),
            cause.to_vec(),
            payload.into(),
        ));
        self.now = self.now.checked_add(SignedDuration::from_secs(1)).unwrap();
        number
    }

    pub(super) fn journal(&self) -> Journal {
        Journal::new(self.events.clone()).unwrap()
    }

    pub(super) fn report(&self) -> String {
        report(&self.journal(), None)
    }
}

pub(super) fn config_loaded(build: Build) -> ConfigLoaded {
    ConfigLoaded {
        build,
        config: Config::default().snapshot(),
        path: "/etc/togi/config.json".to_owned(),
        file: false,
        kernel_cursor: None,
        kernel_error: None,
    }
}

pub(super) fn shutdown(reason: ShutdownReason) -> Shutdown {
    Shutdown {
        reason,
        kernel_cursor: None,
        kernel_error: None,
        cycles: None,
    }
}

/// A trial of the first workload of `regime` loading `cores`, as the tuner would plan it in a search.
pub(super) fn intent(id: &str, regime: Regime, cores: &[i64], offsets: &[i64]) -> TrialIntent {
    TrialIntent {
        trial: TrialId::from(id.to_owned()),
        kernel_cursor: None,
        kernel_error: None,
        profile: profile(offsets),
        core: None,
        cores: set(cores),
        offset: None,
        regime,
        workload: workload(regime, 0),
        duration_s: 120,
        condition: Condition::Together,
        activity: None,
        retry: false,
        cycle: None,
        step: None,
        hunt: None,
        group: None,
        round: None,
        rerun: false,
    }
}

pub(super) fn trial_start(id: &str) -> TrialStart {
    TrialStart {
        trial: TrialId::from(id.to_owned()),
        window_start_ns: None,
        scope: String::new(),
        pid: 1,
        cpus: Vec::new(),
        argv: Vec::new(),
        files: Vec::new(),
        instances: Vec::new(),
    }
}

pub(super) fn end(id: &str, outcome: Outcome) -> TrialEnd {
    TrialEnd {
        trial: TrialId::from(id.to_owned()),
        kernel_cursor: None,
        kernel_error: None,
        outcome,
        signal: (outcome == Outcome::Failure).then_some(Signal::ComputationError),
        core: None,
        duration_s: 60,
        tctl_max_c: None,
        voltage_request_median_v: None,
        voltage_request_min_v: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: std::collections::BTreeMap::new(),
        last_sample_s: None,
        last_sample_tctl_c: None,
        last_sample_min_mhz: None,
        last_sample_max_mhz: None,
        stalled_core: None,
        worker_stalled_ms: None,
        reason: None,
        interrupted: false,
        last_evidence: false,
        escaped: Vec::new(),
        backend_missing: false,
        containment_error: None,
    }
}

pub(super) fn failure(trial: Option<&str>) -> Failure {
    Failure {
        signal: Signal::ComputationError,
        attribution: Attribution::Unattributed,
        core: None,
        offset: None,
        trial: trial.map(|id| TrialId::from(id.to_owned())),
        regime: None,
        condition: None,
        profile: None,
        known_failure: None,
        reason: None,
        round: None,
    }
}

pub(super) fn crash_detected(previous: &str, in_flight: Option<Seq>) -> CrashDetected {
    CrashDetected {
        previous_boot: BootId::from(previous.to_owned()),
        in_flight,
        stray: false,
        condition: None,
        reset_reason: None,
        reset_reason_raw: Vec::new(),
        inconclusive: false,
        pstore: None,
    }
}

pub(super) fn cycle(number: u32, event: CycleEvent, full: bool) -> CheckingCycle {
    CheckingCycle {
        cycle: nz(number),
        event,
        passed: false,
        full,
        missing: Vec::new(),
        steps: Vec::new(),
        reason: None,
    }
}

pub(super) fn hunt_start(hunt: u32, trial: &str, trials: u32) -> HuntStart {
    HuntStart {
        hunt: nz(hunt),
        failure: seq(1),
        trial: Some(TrialId::from(trial.to_owned())),
        regime: Regime::R6,
        workload: workload(Regime::R6, 0),
        cores: set(&[0, 1]),
        duration_s: 120,
        failing: profile(&[0, 0]),
        parked: profile(&[0, 0]),
        parked_seq: None,
        candidates: set(&[0, 1]),
        trials,
        trial_s: 60,
        miss: 0.0,
        rate: 0.0,
        ranking: Vec::new(),
        reason: None,
    }
}

pub(super) fn hunt_group(hunt: u32, group: u32, stage: HuntStage) -> HuntGroup {
    HuntGroup {
        hunt: nz(hunt),
        group: nz(group),
        cores: set(&[0, 1]),
        profile: profile(&[0, 0]),
        set: set(&[0, 1]),
        granularity: 1,
        stage,
        index: 0,
        duration_s: 60,
        escalated: false,
        full_checked: false,
        any_failed: false,
        probe: None,
        held: Vec::new(),
        inferred: None,
        skipped: false,
        reason: None,
    }
}

pub(super) fn hunt_end(hunt: u32, result: HuntOutcome) -> HuntEnd {
    HuntEnd {
        hunt: nz(hunt),
        result,
        cores: CoreSet::default(),
        members: Vec::new(),
        groups: 0,
        reason: String::new(),
    }
}

/// A trial's intent, start and end in `log`, returning the end's seq.
pub(super) fn trial(log: &mut Log, intent: TrialIntent, outcome: Outcome) -> Seq {
    let id = intent.trial.to_string();
    log.add(intent);
    log.add(trial_start(&id));
    log.add(end(&id, outcome))
}

pub(super) fn decision(kind: Decision, core_id: i64, from: i64, to: i64) -> TunerDecision {
    TunerDecision {
        core: core(core_id),
        activity: Activity::Search,
        decision: kind,
        from_offset: target(from),
        to_offset: target(to),
        pass: None,
        failure_point: None,
        workloads: Vec::new(),
        reason: String::new(),
    }
}

/// What `render` writes into a table of one or more sections.
pub(super) fn rendered(sections: impl FnOnce(&mut crate::table::Table)) -> String {
    let mut table = crate::table::Table::default();
    sections(&mut table);
    table.finish()
}

impl Log {
    /// An event a minute after the one before it.
    pub(super) fn spaced(&mut self, payload: impl Into<Payload>) -> Seq {
        let minutes = i64::try_from(self.events.len()).unwrap();
        self.at(minutes * 60).add(payload)
    }
}
