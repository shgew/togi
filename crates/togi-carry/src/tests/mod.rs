mod boundary;
mod cost;
mod facts_walk;
mod purity;
mod resolution;
mod seeds;
mod zero;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::num::NonZeroU32;
use std::rc::Rc;

use jiff::Timestamp;
use togi_config::{Backends, Config};
use togi_defect::{Clause, DefectId, Entry};
use togi_facts::{Fact, FactId};
use togi_journal::{
    Attribution, Build, CarriedCore, CarriedFailurePoint, CarriedLimit, ConfigLoaded, Decision,
    DefectDirection, Event, EventKind, Failure, Outcome, Payload, Provenance, SCHEMA, Seq,
    SessionContext, SessionId, SessionStart, Stamp, TrialEnd, TrialId, TrialIntent, TunerDecision,
};
use togi_machine::{
    BiosContext, BootId, CcdId, Condition, CoreId, CoreInfo, CoreSet, CpuId, PerCore, Regime,
    Signal, TargetOffset, WorkloadId,
};

use crate::{Archives, PendingCarry, ResolvedCarry, copied_identity, select};

pub(super) fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

pub(super) fn set(cores: &[i64]) -> CoreSet {
    cores.iter().map(|&n| core(n)).collect()
}

pub(super) fn offset(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}

pub(super) fn profile(pairs: &[(i64, i64)]) -> PerCore<TargetOffset> {
    pairs.iter().map(|&(n, v)| (core(n), offset(v))).collect()
}

pub(super) fn seq(n: u64) -> Seq {
    Seq::new(n).unwrap()
}

pub(super) fn day(n: u32) -> SessionId {
    format!("202610{n:02}T000000Z").parse().unwrap()
}

pub(super) fn before() -> SessionId {
    session("20260901T000000Z")
}

pub(super) fn session(text: &str) -> SessionId {
    text.parse().unwrap()
}

pub(super) fn context() -> BiosContext {
    BiosContext {
        bios_version: "3.10".into(),
        board: "X870E".into(),
        cpu_model: "9950X".into(),
        microcode: "0xb404032".into(),
        boost_limit_mhz: 5700,
    }
}

pub(super) fn other_context() -> BiosContext {
    BiosContext {
        bios_version: "3.20".into(),
        ..context()
    }
}

pub(super) fn workload(regime: Regime, index: usize) -> WorkloadId {
    WorkloadId::new(regime, index).unwrap()
}

pub(super) fn limit(value: i64, session: SessionId, at: Seq) -> CarriedLimit {
    CarriedLimit {
        offset: offset(value),
        from: Provenance { session, seq: at },
    }
}

pub(super) fn point(
    value: i64,
    session: SessionId,
    at: Seq,
    signal: Signal,
) -> CarriedFailurePoint {
    CarriedFailurePoint {
        offset: offset(value),
        from: Provenance { session, seq: at },
        signal,
    }
}

pub(super) fn seed(
    core_id: i64,
    candidate_solo_limit: Option<CarriedLimit>,
    failure_point: Option<CarriedFailurePoint>,
) -> CarriedCore {
    CarriedCore {
        core: core(core_id),
        candidate_solo_limit,
        failure_point,
    }
}

pub(super) fn build(ruleset: u32, evidence: u32, fixes: u32) -> Build {
    Build {
        version: "0.1.0".into(),
        rev: None,
        ruleset,
        schema: SCHEMA,
        fixes,
        evidence,
    }
}

pub(super) struct Failed {
    pub(super) end: Seq,
    pub(super) failure: Seq,
}

pub(super) struct Source {
    pub(super) id: SessionId,
    pub(super) cores: CoreSet,
    pub(super) events: Vec<Event>,
    topology: Vec<i64>,
    build: Build,
    trials: u32,
}

impl Source {
    pub(super) fn new(id: SessionId, cores: &[i64], ruleset: u32, evidence: u32) -> Self {
        let build = build(ruleset, evidence, 0);
        let mut source = Self {
            id,
            cores: set(cores),
            events: Vec::new(),
            topology: cores.to_vec(),
            build: build.clone(),
            trials: 0,
        };
        source.add(SessionStart {
            build,
            session: id,
            cores: cores
                .iter()
                .map(|&n| CoreInfo {
                    core: core(n),
                    ccd: CcdId::new(0).unwrap(),
                    cpus: vec![CpuId::from(u16::try_from(n).unwrap())],
                })
                .collect(),
        });
        source
    }

    pub(super) fn with_context(mut self, context: &BiosContext) -> Self {
        self.add(SessionContext::from(context.clone()));
        self
    }

    pub(super) fn add(&mut self, payload: impl Into<Payload>) -> Seq {
        self.add_caused(&[], payload)
    }

    pub(super) fn add_caused(&mut self, cause: &[Seq], payload: impl Into<Payload>) -> Seq {
        let at = seq(self.events.len() as u64 + 1);
        let stamp = Stamp {
            time: Timestamp::from_second(1_700_000_000 + i64::try_from(at.get()).unwrap()).unwrap(),
            mono: None,
        };
        self.events.push(Event::new(
            at,
            stamp,
            BootId::from("boot".to_owned()),
            cause.to_vec(),
            payload.into(),
        ));
        at
    }

    pub(super) fn id_of(&self, end: Seq) -> FactId {
        FactId {
            session: self.id,
            seq: end,
        }
    }

    pub(super) fn config_loaded(&mut self, build: Build, config: &Config) -> Seq {
        self.add(ConfigLoaded {
            build,
            config: config.snapshot(),
            path: "config.json".into(),
            file: true,
            kernel_cursor: None,
            kernel_error: None,
        })
    }

    pub(super) fn fixes(&mut self, fixes: u32) -> Seq {
        let build = Build {
            fixes,
            ..self.build.clone()
        };
        self.config_loaded(build, &Config::default())
    }

    pub(super) fn backends(&mut self, mprime: &str, ycruncher: &str) -> Seq {
        let config = Config::parse(&format!(
            r#"{{"backends":{{"mprime":"{mprime}","ycruncher":"{ycruncher}"}}}}"#
        ))
        .unwrap();
        self.config_loaded(self.build.clone(), &config)
    }

    pub(super) fn next_trial(&mut self) -> TrialId {
        self.trials += 1;
        TrialId::from(format!("{:04}", self.trials))
    }

    pub(super) fn trial_intent(
        &self,
        trial: &TrialId,
        core_id: i64,
        value: i64,
        condition: Condition,
        workload: WorkloadId,
    ) -> TrialIntent {
        let profile = self
            .topology
            .iter()
            .map(|&n| {
                let at = match condition {
                    Condition::Alone if n != core_id => 0,
                    Condition::Alone | Condition::Together | Condition::Parked => value,
                };
                (core(n), offset(at))
            })
            .collect();
        TrialIntent {
            trial: trial.clone(),
            kernel_cursor: None,
            kernel_error: None,
            profile,
            core: Some(core(core_id)),
            cores: CoreSet::default(),
            offset: Some(offset(value)),
            regime: workload.regime(),
            workload,
            duration_s: 90,
            condition,
            activity: Some(togi_journal::Activity::Search),
            retry: false,
            cycle: None,
            step: None,
            hunt: None,
            group: None,
            round: None,
            rerun: false,
        }
    }

    pub(super) fn intent(&mut self, core_id: i64, value: i64, condition: Condition) -> TrialId {
        self.intent_on(core_id, value, condition, workload(Regime::R1, 0))
    }

    pub(super) fn intent_on(
        &mut self,
        core_id: i64,
        value: i64,
        condition: Condition,
        workload: WorkloadId,
    ) -> TrialId {
        let trial = self.next_trial();
        let intent = self.trial_intent(&trial, core_id, value, condition, workload);
        self.add(intent);
        trial
    }

    pub(super) fn pass(&mut self, core_id: i64, value: i64, condition: Condition) -> Seq {
        self.pass_on(core_id, value, condition, workload(Regime::R1, 0))
    }

    pub(super) fn pass_on(
        &mut self,
        core_id: i64,
        value: i64,
        condition: Condition,
        workload: WorkloadId,
    ) -> Seq {
        let trial = self.intent_on(core_id, value, condition, workload);
        self.add(trial_end(&trial, Outcome::Pass, None, None))
    }

    pub(super) fn fail(
        &mut self,
        core_id: i64,
        value: i64,
        condition: Condition,
        attribution: Attribution,
    ) -> Failed {
        self.fail_on(
            core_id,
            value,
            condition,
            attribution,
            workload(Regime::R1, 0),
        )
    }

    pub(super) fn fail_on(
        &mut self,
        core_id: i64,
        value: i64,
        condition: Condition,
        attribution: Attribution,
        workload: WorkloadId,
    ) -> Failed {
        let trial = self.intent_on(core_id, value, condition, workload);
        let end = self.add(trial_end(
            &trial,
            Outcome::Failure,
            Some(Signal::UnexpectedExit),
            Some(core(core_id)),
        ));
        let failure = self.add_caused(
            &[end],
            Failure {
                signal: Signal::UnexpectedExit,
                attribution,
                core: Some(core(core_id)),
                offset: Some(offset(value)),
                trial: Some(trial),
                regime: Some(workload.regime()),
                condition: Some(condition),
                profile: None,
                known_failure: None,
                reason: None,
                round: None,
            },
        );
        Failed { end, failure }
    }
}

pub(super) fn trial_end(
    trial: &TrialId,
    outcome: Outcome,
    signal: Option<Signal>,
    core: Option<CoreId>,
) -> TrialEnd {
    TrialEnd {
        trial: trial.clone(),
        kernel_cursor: None,
        kernel_error: None,
        outcome,
        signal,
        core,
        duration_s: if outcome == Outcome::Failure { 11 } else { 90 },
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

pub(super) fn idle_failure(profile: PerCore<TargetOffset>) -> Failure {
    Failure {
        signal: Signal::Crash,
        attribution: Attribution::Unattributed,
        core: None,
        offset: None,
        trial: None,
        regime: None,
        condition: Some(Condition::Together),
        profile: Some(profile),
        known_failure: None,
        reason: None,
        round: None,
    }
}

pub(super) fn backoff_decision(core_id: i64) -> TunerDecision {
    TunerDecision {
        core: core(core_id),
        activity: togi_journal::Activity::Search,
        decision: Decision::Backoff,
        from_offset: offset(-25),
        to_offset: offset(-24),
        pass: None,
        failure_point: None,
        workloads: Vec::new(),
        reason: "backoff".into(),
    }
}

const BACKOFF_ON_FAILURE: Clause = Clause {
    decision_kind: EventKind::TunerDecision,
    decision: Decision::Backoff,
    cause_kind: EventKind::Failure,
};

pub(super) static DEFECTS: [Entry; 1] = [Entry {
    id: DefectId::new(1).unwrap(),
    title: "synthetic",
    detail: "synthetic detail",
    pr: 100,
    direction: DefectDirection::TooCautious,
    clauses: &[BACKOFF_ON_FAILURE],
}];

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(super) struct Unreadable(pub(super) &'static str);

enum Stored {
    Events(Vec<Event>),
    Poisoned,
    Corrupt,
    Unopenable,
}

#[derive(Default)]
pub(super) struct Archive {
    stored: BTreeMap<SessionId, Stored>,
    pub(super) opened: Vec<SessionId>,
    polled: Rc<RefCell<BTreeMap<SessionId, usize>>>,
}

impl Archive {
    pub(super) fn of(sources: &[&Source]) -> Self {
        let mut archive = Self::default();
        for source in sources {
            archive.put(source);
        }
        archive
    }

    pub(super) fn put(&mut self, source: &Source) {
        self.stored
            .insert(source.id, Stored::Events(source.events.clone()));
    }

    pub(super) fn poison(&mut self, id: SessionId) {
        self.stored.insert(id, Stored::Poisoned);
    }

    pub(super) fn corrupt(&mut self, id: SessionId) {
        self.stored.insert(id, Stored::Corrupt);
    }

    pub(super) fn unopenable(&mut self, id: SessionId) {
        self.stored.insert(id, Stored::Unopenable);
    }

    pub(super) fn polled(&self, id: SessionId) -> usize {
        self.polled.borrow().get(&id).copied().unwrap_or(0)
    }
}

impl Archives for Archive {
    type Error = Unreadable;
    type Events = Box<dyn Iterator<Item = Result<Event, Unreadable>>>;

    fn events(&mut self, session: SessionId) -> Result<Option<Self::Events>, Unreadable> {
        self.opened.push(session);
        Ok(match self.stored.get(&session) {
            None => None,
            Some(Stored::Events(events)) => {
                let polled = Rc::clone(&self.polled);
                Some(Box::new(events.clone().into_iter().map(Ok).inspect(
                    move |_| *polled.borrow_mut().entry(session).or_default() += 1,
                )))
            }
            Some(Stored::Poisoned) => Some(Box::new(std::iter::from_fn(
                || -> Option<Result<Event, Unreadable>> {
                    panic!("a source that must stay unread was polled")
                },
            ))),
            Some(Stored::Unopenable) => return Err(Unreadable("unopenable")),
            Some(Stored::Corrupt) => Some(Box::new(std::iter::once(Err(Unreadable("corrupt"))))),
        })
    }
}

pub(super) fn pending(
    archive: &mut Archive,
    chain: &[SessionId],
    boundary: Option<SessionId>,
    registry: &'static [Entry],
) -> PendingCarry {
    select(chain, boundary, registry, archive).unwrap().unwrap()
}

pub(super) fn chain_of(sources: &[&Source]) -> (Archive, Vec<SessionId>) {
    (
        Archive::of(sources),
        sources.iter().map(|source| source.id).collect(),
    )
}

pub(super) fn resolve(pending: &PendingCarry, epoch: u32) -> ResolvedCarry {
    pending.resolve(&context(), &Backends::default(), epoch)
}

pub(super) fn carry_into(target: &mut Source, resolved: &ResolvedCarry, commit: bool) {
    let copied: HashSet<FactId> = target
        .events
        .iter()
        .filter_map(|event| copied_identity(&event.payload))
        .collect();
    for payload in resolved.fact_payloads(&copied, target.cores) {
        target.add(payload);
    }
    if commit {
        target.add(resolved.session_carried(target.cores));
    }
}

pub(super) fn ids(facts: &[Fact]) -> Vec<FactId> {
    facts.iter().map(Fact::id).collect()
}

pub(super) fn hunt_start(
    hunt: u32,
    failure: Seq,
    failing: PerCore<TargetOffset>,
    candidates: CoreSet,
) -> togi_journal::HuntStart {
    togi_journal::HuntStart {
        hunt: NonZeroU32::new(hunt).unwrap(),
        failure,
        trial: None,
        regime: Regime::R7,
        workload: workload(Regime::R7, 0),
        cores: CoreSet::default(),
        duration_s: 120,
        failing,
        parked: PerCore::default(),
        parked_seq: None,
        candidates,
        trials: 3,
        trial_s: 30,
        miss: 0.01,
        rate: 0.5,
        ranking: Vec::new(),
        reason: None,
    }
}

pub(super) fn culprit(hunt: u32, cores: CoreSet) -> togi_journal::HuntEnd {
    togi_journal::HuntEnd {
        hunt: NonZeroU32::new(hunt).unwrap(),
        result: togi_journal::HuntOutcome::Culprit,
        cores,
        members: Vec::new(),
        groups: 1,
        reason: "culprit".into(),
    }
}

pub(super) fn reset(target: togi_journal::ResetTarget) -> togi_journal::CommandReset {
    togi_journal::CommandReset { target }
}

pub(super) fn summary_of(
    source: &Source,
) -> Result<crate::SourceSummary, togi_facts::FactsError<std::convert::Infallible>> {
    crate::summary::summarize(
        source.id,
        source
            .events
            .iter()
            .cloned()
            .map(Ok::<_, std::convert::Infallible>),
        &[],
        None,
    )
}
