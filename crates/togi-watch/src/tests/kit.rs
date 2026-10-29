//! Journals the tests fold: a script appends events as a session would, and a history harness feeds events to
//! the facts alone for the tests of what the history says.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::Timestamp;
use togi_config::Config;
use togi_journal::{
    Activity, Build, CcdVector, ConfigLoaded, CoreTransition, Event, FixStamp, Outcome, Payload,
    SCHEMA, Seq, SessionStart, SessionWarning, Stamp, State as JournalState, TrialEnd, TrialId,
    TrialIntent,
};
use togi_machine::{
    BootId, CcdId, CcdProfile, Condition, CoreId, CoreInfo, CoreSet, ObservedOffset, PerCore,
    Regime, Signal, TargetOffset, workloads,
};

use crate::facts::Facts;
use crate::model::Model;

pub(crate) fn core(id: u8) -> CoreId {
    CoreId::new(i64::from(id)).unwrap()
}

pub(crate) fn off(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}

pub(crate) fn set(ids: &[u8]) -> CoreSet {
    ids.iter().map(|&id| core(id)).collect()
}

pub(crate) fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}

pub(crate) fn seq(value: u64) -> Seq {
    Seq::new(value).unwrap()
}

pub(crate) fn build() -> Build {
    Build {
        version: "0.1.0".into(),
        rev: None,
        ruleset: togi_tuner::RULESET,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: togi_tuner::EVIDENCE_EPOCH,
    }
}

pub(crate) fn infos(cores: &[(u8, u8)]) -> Vec<CoreInfo> {
    cores
        .iter()
        .map(|&(id, ccd)| CoreInfo {
            core: core(id),
            ccd: CcdId::try_from(ccd).unwrap(),
            cpus: Vec::new(),
        })
        .collect()
}

/// `ccd` over a topology of one core per CCD, each core numbered as its CCD.
pub(crate) fn expanded(ccd: CcdProfile) -> CcdVector {
    let cores: Vec<(u8, u8)> = ccd.ccds().map(|id| (id.get(), id.get())).collect();
    CcdVector::expand(ccd, &infos(&cores)).unwrap()
}

pub(crate) fn session_start(cores: &[(u8, u8)]) -> Payload {
    SessionStart {
        build: build(),
        session: "20231114T221320Z".parse().unwrap(),
        cores: infos(cores),
    }
    .into()
}

pub(crate) fn config_loaded() -> Payload {
    ConfigLoaded {
        build: build(),
        config: Config::default().snapshot(),
        path: "/etc/togi/config.json".to_owned(),
        file: false,
        kernel_cursor: None,
        kernel_error: None,
    }
    .into()
}

/// How a core stands when a script's session starts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Start {
    pub(crate) from: Option<togi_journal::CorePhase>,
    pub(crate) to: togi_journal::CorePhase,
    pub(crate) offset: i64,
    pub(crate) pass: Option<i64>,
    pub(crate) fail: Option<i64>,
}

impl Start {
    pub(crate) fn search(offset: i64) -> Self {
        Self {
            from: None,
            to: togi_journal::CorePhase::Search,
            offset,
            pass: None,
            fail: None,
        }
    }
}

pub(crate) fn transition(id: u8, start: Start) -> CoreTransition {
    CoreTransition {
        core: core(id),
        from: start.from,
        to: start.to,
        offset: off(start.offset),
        pass: start.pass.map(off),
        failure_point: start.fail.map(off),
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    }
}

/// An event journal that grows as a session would append it.
#[derive(Clone)]
pub(crate) struct Script {
    pub(crate) events: Vec<Event>,
    pub(crate) boot: String,
    trials: u32,
}

impl Script {
    /// A session over `cores` (id, CCD) with its config loaded.
    pub(crate) fn new(cores: &[(u8, u8)]) -> Self {
        let mut script = Self {
            events: Vec::new(),
            boot: "boot".to_owned(),
            trials: 0,
        };
        script.append(session_start(cores), &[]);
        script.append(config_loaded(), &[]);
        script
    }

    /// Three cores on two CCDs.
    pub(crate) fn three() -> Self {
        Self::new(&[(0, 0), (1, 0), (2, 1)])
    }

    pub(crate) fn next_seq(&self) -> Seq {
        seq(self.events.len() as u64 + 1)
    }

    pub(crate) fn event(&self, payload: Payload, cause: &[Seq]) -> Event {
        let at = self.next_seq();
        let time =
            Timestamp::from_second(1_700_000_000 + i64::try_from(at.get()).unwrap()).unwrap();
        Event::new(
            at,
            Stamp { time, mono: None },
            BootId::from(self.boot.clone()),
            cause.to_vec(),
            payload,
        )
    }

    pub(crate) fn append(&mut self, payload: Payload, cause: &[Seq]) -> Seq {
        let event = self.event(payload, cause);
        let at = event.seq;
        self.events.push(event);
        at
    }

    /// Records the intent of the next trial: `regime` on `cores`, every core at -10.
    pub(crate) fn intent(&mut self, regime: Regime, cores: &[u8]) -> (Seq, TrialId) {
        self.trials += 1;
        let trial = format!("{:04}", self.trials);
        let mut intent = intent(&trial, regime, Condition::Together, &[], cores);
        intent.profile = cores.iter().map(|&id| (core(id), off(-10))).collect();
        intent.activity = Some(Activity::Mapping);
        (self.append(intent.into(), &[]), TrialId::from(trial))
    }

    pub(crate) fn start(&mut self, trial: &TrialId) -> Seq {
        self.append(trial_start(trial), &[])
    }

    pub(crate) fn end(&mut self, intent: Seq, trial: &TrialId, outcome: Outcome) -> Seq {
        let signal = (outcome == Outcome::Failure).then_some(Signal::ComputationError);
        self.append(end_payload(trial, outcome, signal).into(), &[intent])
    }

    /// Runs one trial from intent to end.
    pub(crate) fn run(&mut self, regime: Regime, cores: &[u8], outcome: Outcome) -> TrialId {
        let (intent, trial) = self.intent(regime, cores);
        self.start(&trial);
        self.end(intent, &trial, outcome);
        trial
    }

    pub(crate) fn model(&self) -> Model {
        let mut model = Model::new();
        for event in &self.events {
            model.fold(event).unwrap();
        }
        model
    }
}

pub(crate) fn trial_start(trial: &TrialId) -> Payload {
    togi_journal::TrialStart {
        trial: trial.clone(),
        window_start_ns: None,
        scope: "scope".to_owned(),
        pid: 1,
        cpus: Vec::new(),
        argv: Vec::new(),
        files: Vec::new(),
        instances: Vec::new(),
        roles: Vec::new(),
        controller: None,
    }
    .into()
}

pub(crate) fn end_payload(trial: &TrialId, outcome: Outcome, signal: Option<Signal>) -> TrialEnd {
    TrialEnd {
        trial: trial.clone(),
        kernel_cursor: None,
        kernel_error: None,
        excluded: Vec::new(),
        outcome,
        signal,
        core: None,
        duration_s: 60,
        tctl_max_c: None,
        voltage_request_median_v: None,
        voltage_request_min_v: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
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

pub(crate) fn readback(id: u8, offset: i64) -> Payload {
    togi_journal::SmuReadback {
        core: core(id),
        offset: ObservedOffset(offset),
        expected: None,
    }
    .into()
}

pub(crate) fn warning(text: &str) -> Payload {
    SessionWarning {
        operation: "projection".to_owned(),
        trial: None,
        error: text.to_owned(),
    }
    .into()
}

/// Feeds events to the facts alone, so a test can say what the history makes of a journal the tuner would refuse.
pub(crate) struct Hist {
    pub(crate) journal: JournalState,
    pub(crate) facts: Facts,
    pub(crate) boot: String,
    pub(crate) count: u64,
}

impl Hist {
    pub(crate) fn new(cores: &[(u8, u8)]) -> Self {
        let mut hist = Self {
            journal: JournalState::default(),
            facts: Facts::default(),
            boot: "boot".to_owned(),
            count: 0,
        };
        hist.push(session_start(cores));
        hist
    }

    pub(crate) fn three() -> Self {
        Self::new(&[(0, 0), (1, 0), (2, 1)])
    }

    pub(crate) fn push(&mut self, payload: Payload) -> Seq {
        self.push_with(payload, &[])
    }

    pub(crate) fn push_with(&mut self, payload: Payload, cause: &[Seq]) -> Seq {
        self.count += 1;
        let at = seq(self.count);
        let time = Timestamp::from_second(1000 + i64::try_from(self.count).unwrap()).unwrap();
        let event = Event::new(
            at,
            Stamp { time, mono: None },
            BootId::from(self.boot.clone()),
            cause.to_vec(),
            payload,
        );
        self.journal.apply(&event).unwrap();
        self.facts.fold(&event, &self.journal);
        at
    }

    /// The history as `tag: sentence`, oldest first.
    pub(crate) fn sentences(&self) -> Vec<String> {
        let (mut entries, _) = self.facts.history.recent();
        entries.reverse();
        entries.iter().map(sentence).collect()
    }
}

pub(crate) fn sentence(entry: &crate::history::Entry) -> String {
    let (before, alarm, after) = entry.sentence();
    format!("{}: {before}{alarm}{after}", entry.tag.word())
}

pub(crate) fn intent(
    trial: &str,
    regime: Regime,
    condition: Condition,
    profile: &[i64],
    cores: &[u8],
) -> TrialIntent {
    TrialIntent {
        task_start: None,
        night: None,
        task: None,
        content: Vec::new(),
        placement: None,
        tested: None,
        trial: TrialId::from(trial.to_owned()),
        kernel_cursor: None,
        kernel_error: None,
        profile: profile
            .iter()
            .enumerate()
            .map(|(id, &offset)| (core(u8::try_from(id).unwrap()), off(offset)))
            .collect(),
        core: None,
        cores: set(cores),
        offset: None,
        regime,
        workload: workloads(regime)[0].id(),
        duration_s: 120,
        condition,
        activity: Some(Activity::Checking),
        retry: false,
        cycle: None,
        step: None,
        hunt: None,
        group: None,
        round: None,
        rerun: false,
    }
}

pub(crate) fn smu_intent(core: u8, offset: i64) -> Payload {
    Payload::SmuIntent(
        serde_json::from_str(&format!(
            r#"{{"op":"set","core":{core},"offset":{offset}}}"#
        ))
        .unwrap(),
    )
}

pub(crate) fn preflight() -> Payload {
    togi_journal::PreflightCheck {
        check: "check".to_owned(),
        detail: String::new(),
        ok: true,
    }
    .into()
}

pub(crate) fn offsets(values: &[i64]) -> PerCore<TargetOffset> {
    values
        .iter()
        .enumerate()
        .map(|(id, &offset)| (core(u8::try_from(id).unwrap()), off(offset)))
        .collect()
}

pub(crate) fn crash(previous: &str, in_flight: Option<Seq>) -> togi_journal::CrashDetected {
    togi_journal::CrashDetected {
        previous_boot: BootId::from(previous.to_owned()),
        in_flight,
        stray: false,
        condition: Some(Condition::Together),
        reset_reason: None,
        reset_reason_raw: Vec::new(),
        pstore: None,
        inconclusive: false,
    }
}

pub(crate) fn hunt_start(hunt: u32, candidates: &[u8]) -> togi_journal::HuntStart {
    togi_journal::HuntStart {
        hunt: nz(hunt),
        failure: seq(1),
        trial: None,
        regime: Regime::R7,
        workload: workloads(Regime::R7)[0].id(),
        cores: set(&[0, 1, 2]),
        duration_s: 120,
        failing: offsets(&[-20, -30, -10]),
        parked: offsets(&[-10, -30, -5]),
        parked_seq: None,
        candidates: set(candidates),
        trials: 5,
        trial_s: 120,
        miss: 0.0,
        rate: 0.0,
        ranking: Vec::new(),
        reason: None,
    }
}

pub(crate) fn cycle(cycle: u32, event: togi_journal::CycleEvent) -> togi_journal::CheckingCycle {
    togi_journal::CheckingCycle {
        cycle: nz(cycle),
        event,
        passed: false,
        full: false,
        missing: Vec::new(),
        steps: Vec::new(),
        reason: None,
    }
}

pub(crate) fn failure(signal: Signal, trial: Option<&str>) -> togi_journal::Failure {
    togi_journal::Failure {
        signal,
        attribution: togi_journal::Attribution::Unattributed,
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

/// When the event with this sequence number happened in a [`Script`].
pub(crate) fn at(seq: Seq) -> Timestamp {
    Timestamp::from_second(1_700_000_000 + i64::try_from(seq.get()).unwrap()).unwrap()
}
