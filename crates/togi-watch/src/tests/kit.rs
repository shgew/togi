//! Journals the tests fold: a shadow tuner drives the intents so every journal is one the native tuner accepts,
//! and a history harness feeds events to the facts alone for the tests of what the history says.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::Timestamp;
use togi_config::Config;
use togi_journal::{
    Activity, Build, ConfigLoaded, CoreTransition, Event, Outcome, Payload, SCHEMA, Seq,
    SessionStart, SessionWarning, Stamp, State as JournalState, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{
    BootId, CcdId, Condition, CoreId, CoreInfo, CoreSet, ObservedOffset, PerCore, Regime, Signal,
    TargetOffset, workloads,
};
use togi_tuner::{Action, State as TunerState, TrialPlan};

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
        fixes: 0,
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

    pub(crate) fn at_limit(offset: i64, fail: i64) -> Self {
        Self {
            from: None,
            to: togi_journal::CorePhase::AtLimit,
            offset,
            pass: None,
            fail: Some(fail),
        }
    }

    pub(crate) fn has_room(offset: i64) -> Self {
        Self {
            from: None,
            to: togi_journal::CorePhase::HasRoom,
            offset,
            pass: None,
            fail: None,
        }
    }

    pub(crate) fn solo(offset: i64, solo: i64) -> Self {
        Self {
            from: Some(togi_journal::CorePhase::Search),
            pass: Some(solo),
            fail: (solo > -50).then_some(solo - 1),
            ..Self::has_room(offset)
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

/// An event journal that grows as a session would append it, every event folded into a shadow tuner first.
#[derive(Clone)]
pub(crate) struct Script {
    pub(crate) events: Vec<Event>,
    pub(crate) shadow: TunerState,
    pub(crate) boot: String,
    pub(crate) ids: Vec<u8>,
    trials: u32,
}

impl Script {
    /// A session over `cores` (id, CCD), each core starting as `starts` says.
    pub(crate) fn new(cores: &[(u8, u8)], starts: &[Start]) -> Self {
        let mut script = Self {
            events: Vec::new(),
            shadow: TunerState::new(),
            boot: "boot".to_owned(),
            ids: cores.iter().map(|&(id, _)| id).collect(),
            trials: 0,
        };
        let begin = script.append(session_start(cores), &[]);
        script.append(config_loaded(), &[]);
        for (&(id, _), start) in cores.iter().zip(starts) {
            script.append(transition(id, *start).into(), &[begin]);
        }
        script
    }

    /// Three cores on two CCDs at a limit of -30, the way a hunt's session begins.
    pub(crate) fn at_limit() -> Self {
        let mut script = Self::new(&[(0, 0), (1, 0), (2, 1)], &[Start::at_limit(-30, -31); 3]);
        script.append(
            togi_journal::ProfileChange {
                from: None,
                to: [(0u8, -30i64), (1, -30), (2, -30)]
                    .iter()
                    .map(|&(id, offset)| (core(id), off(offset)))
                    .collect(),
            }
            .into(),
            &[],
        );
        script.ranking();
        script
    }

    pub(crate) fn ranking(&mut self) -> Seq {
        let ranking = self.ids.iter().map(|&id| core(id)).collect();
        self.append(
            togi_journal::HostRanking {
                ranking,
                values: PerCore::default(),
                detail: None,
            }
            .into(),
            &[],
        )
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
        self.shadow.fold(&event).unwrap();
        let at = event.seq;
        self.events.push(event);
        at
    }

    pub(crate) fn plan(&self) -> TrialPlan {
        match self.shadow.next().expect("the tuner has a next action") {
            Action::RunTrial(plan) => plan,
            other @ (Action::Decide(_) | Action::ReadRanking) => {
                panic!("expected a trial, the tuner asked for {other:?}")
            }
        }
    }

    /// Records the tuner\'s decisions until it asks for a trial, and returns that trial.
    pub(crate) fn next_trial(&mut self) -> TrialPlan {
        for _ in 0..50 {
            match self.shadow.next().expect("the tuner has a next action") {
                Action::RunTrial(plan) => return plan,
                Action::Decide(_) | Action::ReadRanking => {
                    self.step(|_| Outcome::Pass);
                }
            }
        }
        panic!("the tuner asks for no trial");
    }

    /// Records the tuner's next action: a decision is appended, a trial runs to `outcome`. Returns what was done.
    pub(crate) fn step(&mut self, mut outcome: impl FnMut(&TrialPlan) -> Outcome) -> Action {
        let action = self.shadow.next().expect("the tuner has a next action");
        match &action {
            Action::Decide(decision) => {
                self.append(decision.payload.clone().into(), &decision.cause);
            }
            Action::RunTrial(plan) => {
                let result = outcome(plan);
                let (intent, trial) = self.intent(plan);
                self.end(intent, &trial, result);
            }
            Action::ReadRanking => {
                self.ranking();
            }
        }
        action
    }

    /// The intent of `plan`: the first workload rotation the tuner accepts.
    pub(crate) fn intent(&mut self, plan: &TrialPlan) -> (Seq, TrialId) {
        self.trials += 1;
        let trial = TrialId::from(format!("{:04}", self.trials));
        for rotation in 0..16 {
            let payload: Payload = plan.intent(trial.clone(), rotation).into();
            let event = self.event(payload.clone(), &plan.cause);
            if self.shadow.clone().fold(&event).is_ok() {
                return (self.append(payload, &plan.cause), trial);
            }
        }
        panic!("the tuner accepts no workload rotation of {plan:?}");
    }

    pub(crate) fn start(&mut self, trial: &TrialId) -> Seq {
        self.append(trial_start(trial), &[])
    }

    pub(crate) fn end(&mut self, intent: Seq, trial: &TrialId, outcome: Outcome) -> Seq {
        let signal = (outcome == Outcome::Failure).then_some(Signal::ComputationError);
        self.append(end_payload(trial, outcome, signal).into(), &[intent])
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
    }
    .into()
}

pub(crate) fn end_payload(trial: &TrialId, outcome: Outcome, signal: Option<Signal>) -> TrialEnd {
    TrialEnd {
        trial: trial.clone(),
        kernel_cursor: None,
        kernel_error: None,
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
    pub(crate) tuner: TunerState,
    pub(crate) facts: Facts,
    pub(crate) boot: String,
    pub(crate) count: u64,
}

impl Hist {
    pub(crate) fn new(cores: &[(u8, u8)]) -> Self {
        Self::with(cores, crate::history::History::default())
    }

    fn with(cores: &[(u8, u8)], history: crate::history::History) -> Self {
        let mut hist = Self {
            journal: JournalState::default(),
            tuner: TunerState::new(),
            facts: Facts {
                history,
                ..Facts::default()
            },
            boot: "boot".to_owned(),
            count: 0,
        };
        hist.push(session_start(cores));
        hist
    }

    pub(crate) fn unbounded_three() -> Self {
        Self::with(
            &[(0, 0), (1, 0), (2, 1)],
            crate::history::History::unbounded(),
        )
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
        self.facts.fold(&event, &self.journal, &self.tuner);
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

pub(crate) fn hunt_group(hunt: u32, group: u32, cores: &[u8]) -> togi_journal::HuntGroup {
    togi_journal::HuntGroup {
        hunt: nz(hunt),
        group: nz(group),
        cores: set(cores),
        profile: offsets(&[-20, -30, -10]),
        set: set(cores),
        granularity: 1,
        stage: togi_journal::HuntStage::Part,
        index: 0,
        duration_s: 120,
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

pub(crate) fn hunt_end(
    hunt: u32,
    result: togi_journal::HuntOutcome,
    cores: &[u8],
) -> togi_journal::HuntEnd {
    togi_journal::HuntEnd {
        hunt: nz(hunt),
        result,
        cores: set(cores),
        members: Vec::new(),
        groups: 0,
        reason: String::new(),
    }
}

pub(crate) fn combination(
    id: u32,
    hunt: u32,
    members: &[(u8, i64)],
    fallback: bool,
) -> togi_journal::Combination {
    togi_journal::Combination {
        combination: nz(id),
        members: members
            .iter()
            .map(|&(id, offset)| togi_journal::Member {
                core: core(id),
                offset: off(offset),
            })
            .collect(),
        fallback,
        hunt: nz(hunt),
        reason: String::new(),
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

pub(crate) fn fact_source() -> togi_journal::FactSource {
    togi_journal::FactSource {
        session: "20231114T221320Z".parse().unwrap(),
        seq: seq(1),
        build: build(),
        trial: None,
        time: Timestamp::UNIX_EPOCH,
        boot: BootId::from("earlier".to_owned()),
        backend: None,
    }
}

pub(crate) fn carried_trial() -> togi_journal::TrialCarried {
    togi_journal::TrialCarried {
        source: fact_source(),
        class: togi_journal::TrialClass {
            regime: Regime::R7,
            workload: Some(workloads(Regime::R7)[0].id()),
            cores: set(&[0, 1, 2]),
            duration_s: 120,
        },
        condition: Condition::Together,
        activity: None,
        rerun: false,
        profile: offsets(&[-20, -30, -10]),
        outcome: Outcome::Failure,
        signal: Some(Signal::ComputationError),
        duration_s: 30,
        core: None,
        stalled_core: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    }
}

pub(crate) fn carried_failure() -> togi_journal::FailureCarried {
    let mut failure = failure(Signal::Crash, None);
    failure.core = Some(core(1));
    togi_journal::FailureCarried {
        source: fact_source(),
        class: togi_journal::TrialClass {
            regime: Regime::R6,
            workload: None,
            cores: set(&[0, 1, 2]),
            duration_s: 120,
        },
        failure,
    }
}
