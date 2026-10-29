use std::collections::BTreeMap;

use jiff::Timestamp;
use togi_journal::{
    Activity, Binding, Build, Candidate, CcdCap, CcdEdge, CcdVector, CellEvidence, CellWitness,
    DayBootStart, Edge, Event, ExposureClass, ExposureObserved, ExposureOrigin, Finite, FixStamp,
    NightQualification, OriginalSource, Outcome, Payload, Placement, QualificationPurpose,
    QualificationState, Rate, SCHEMA, Seq, SessionBaseline, SessionContext, SessionStart, Slope,
    Stamp, StepKind, TaskId, Tested, TrialEnd, TrialId, TrialStart, WorkloadContent,
};
use togi_machine::{
    Backend, BootId, CcdId, CcdProfile, ContentId, CoreId, CoreInfo, CpuId, Digest, ObservedOffset,
    PerCore, Purpose, Regime, TargetOffset, WorkloadId,
};
use togi_tuner::night::{Cell, Roles, SEARCH_DURATION, Workloads, write_limit};
use togi_tuner::policy::{
    Action, Contents, DayQuery, Decision, EndReason, NightQuery, NotReady, Params, State, TrialPlan,
};

use crate::flow::{apply, cold_boot, steps};

pub(crate) const START: i64 = 1_700_000_000;
/// The session every journal starts with.
pub(crate) const SESSION: &str = "20231114T221320Z";

pub(crate) fn at(second: i64) -> Timestamp {
    Timestamp::from_second(START + second).unwrap()
}

fn build() -> Build {
    Build {
        version: "0.1.0".into(),
        rev: None,
        ruleset: 13,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: 1,
    }
}

pub(crate) fn ccd(id: u8) -> CcdId {
    CcdId::try_from(id).unwrap()
}

/// The all-zero profile of both CCDs.
pub(crate) fn zero() -> CcdProfile {
    CcdProfile::zero([ccd(0), ccd(1)]).unwrap()
}

/// Each core's observed offset under `profile`.
pub(crate) fn offsets_of(profile: &CcdProfile) -> PerCore<ObservedOffset> {
    offsets_on(&cores(), profile)
}

/// Each of `machine`'s cores' observed offset under `profile`.
fn offsets_on(machine: &[CoreInfo], profile: &CcdProfile) -> PerCore<ObservedOffset> {
    machine
        .iter()
        .map(|info| {
            let offset = profile.get(info.ccd).map_or(0, TargetOffset::get);
            (info.core, ObservedOffset(offset))
        })
        .collect()
}

fn context() -> SessionContext {
    firmware("1")
}

/// The session context under firmware `bios_version`.
pub(crate) fn firmware(bios_version: &str) -> SessionContext {
    SessionContext {
        bios_version: bios_version.into(),
        board: "board".into(),
        cpu_model: "cpu".into(),
        microcode: "mc".into(),
        boost_limit_mhz: 5000,
    }
}

/// The machine's four cores, two per CCD, each with two CPUs.
pub(crate) fn cores() -> Vec<CoreInfo> {
    cores_per_ccd(2)
}

/// A machine of two CCDs with `per_ccd` cores each, each core with two CPUs.
pub(crate) fn cores_per_ccd(per_ccd: u8) -> Vec<CoreInfo> {
    (0..2 * per_ccd)
        .map(|id| CoreInfo {
            core: CoreId::new(i64::from(id)).unwrap(),
            ccd: CcdId::try_from(id / per_ccd).unwrap(),
            cpus: vec![
                CpuId::new(i64::from(id)).unwrap(),
                CpuId::new(i64::from(id) + 32).unwrap(),
            ],
        })
        .collect()
}

/// A journal and the policy state folded from it as it grows.
pub(crate) struct Journal {
    pub(crate) events: Vec<Event>,
    pub(crate) state: State,
    /// The boot new events are recorded in.
    boot: String,
    /// Seconds every later event's time is shifted by, as after the days `advance` skipped.
    shift: i64,
    /// The content every workload runs from now on.
    built: ContentId,
    /// The machine's cores, as every session start lists them.
    machine: Vec<CoreInfo>,
}

impl Journal {
    /// An empty journal of the machine `machine` folded by a policy with `params`.
    fn bare_on(params: Params, machine: Vec<CoreInfo>) -> Self {
        Self {
            events: Vec::new(),
            state: State::new(params),
            boot: "boot".to_owned(),
            shift: 0,
            built: content(),
            machine,
        }
    }

    pub(crate) fn append(&mut self, payload: Payload, cause: &[Seq]) -> Seq {
        let boot = self.boot.clone();
        self.append_in(&boot, payload, cause)
    }

    /// Records every later event in `boot`, as after a reboot.
    pub(crate) fn reboot(&mut self, boot: &str) {
        boot.clone_into(&mut self.boot);
    }

    /// Runs every workload from `built` from now on, as after a rebuild of every backend.
    pub(crate) fn rebuild(&mut self, built: ContentId) {
        self.built = built;
    }

    /// Moves the clock of every later event `secs` ahead.
    pub(crate) fn advance(&mut self, secs: i64) {
        self.shift += secs;
    }

    /// The time just after the latest event.
    pub(crate) fn now(&self) -> Timestamp {
        at(i64::try_from(self.events.len()).unwrap() + 1 + self.shift)
    }

    /// Appends `payload` as recorded in `boot`.
    fn append_in(&mut self, boot: &str, payload: Payload, cause: &[Seq]) -> Seq {
        let seq = Seq::new(self.events.len() as u64 + 1).unwrap();
        let event = Event::new(
            seq,
            Stamp {
                time: at(i64::try_from(seq.get()).unwrap() + self.shift),
                mono: None,
            },
            BootId::from(boot.to_owned()),
            cause.to_vec(),
            payload,
        );
        self.state.fold(&event).unwrap();
        self.events.push(event);
        seq
    }

    pub(crate) fn started() -> Self {
        Self::started_with(Params::SPEC)
    }

    fn started_with(params: Params) -> Self {
        Self::started_on(params, cores())
    }

    fn started_on(params: Params, machine: Vec<CoreInfo>) -> Self {
        let mut journal = Self::bare_on(params, machine);
        let cores = journal.machine.clone();
        journal.append(
            SessionStart {
                build: build(),
                session: SESSION.parse().unwrap(),
                cores,
            }
            .into(),
            &[],
        );
        journal
    }

    /// Starts the fresh session `session` on the same machine and firmware.
    pub(crate) fn start_session(&mut self, session: &str) {
        let cores = self.machine.clone();
        self.append(
            SessionStart {
                build: build(),
                session: session.parse().unwrap(),
                cores,
            }
            .into(),
            &[],
        );
        self.append(context().into(), &[]);
    }

    pub(crate) fn contextual() -> Self {
        Self::contextual_with(Params::SPEC)
    }

    fn contextual_with(params: Params) -> Self {
        Self::contextual_on(params, cores())
    }

    fn contextual_on(params: Params, machine: Vec<CoreInfo>) -> Self {
        let mut journal = Self::started_on(params, machine);
        journal.append(context().into(), &[]);
        journal
    }

    /// Reboots into the normal boot `boot` and appends its observer start, at all-zero offsets.
    pub(crate) fn boot_start(&mut self, boot: &str) -> Seq {
        self.boot_start_at(boot, "1", &zero())
    }

    /// Reboots into the normal boot `boot` under firmware `bios_version` and appends its observer start,
    /// each core at its CCD's offset in `profile`.
    pub(crate) fn boot_start_at(
        &mut self,
        boot: &str,
        bios_version: &str,
        profile: &CcdProfile,
    ) -> Seq {
        self.reboot(boot);
        let offsets = offsets_on(&self.machine, profile);
        self.append(
            DayBootStart {
                build: build(),
                bios: firmware(bios_version).bios_context(),
                offsets,
                watchdog: None,
            }
            .into(),
            &[],
        )
    }

    /// Asks the day query until it owes nothing, committing each decision, and returns what it committed.
    pub(crate) fn settle_day(&mut self, query: &DayQuery) -> Vec<Payload> {
        let mut committed = Vec::new();
        for _ in 0..50 {
            let owed = self.state.day(query).unwrap();
            if owed.is_empty() {
                return committed;
            }
            for decision in owed {
                let payload: Payload = decision.payload.into();
                committed.push(payload.clone());
                self.append(payload, &decision.causes);
            }
        }
        panic!("the day kept deciding: {committed:?}");
    }

    pub(crate) fn ready() -> Self {
        Self::ready_with(Params::SPEC)
    }

    /// A journal ready for its first night, folded by a policy with `params`.
    pub(crate) fn ready_with(params: Params) -> Self {
        Self::ready_on(params, cores())
    }

    /// A journal of the machine `machine` ready for its first night, folded by a policy with `params`.
    pub(crate) fn ready_on(params: Params, machine: Vec<CoreInfo>) -> Self {
        let mut journal = Self::contextual_on(params, machine);
        let offsets = offsets_on(&journal.machine, &zero());
        journal.append(SessionBaseline { offsets }.into(), &[]);
        journal
    }

    /// The stock dead end the journal establishes before its owed commitments are drained.
    pub(crate) fn stock_dead_end(&self, now: Timestamp) -> Result<Option<Decision>, NotReady> {
        let contents = Contents::new(std::iter::empty());
        self.state.stock_dead_end(&NightQuery {
            now,
            contents: &contents,
            unavailable: &BTreeMap::new(),
        })
    }

    pub(crate) fn next(&self, now: Timestamp) -> Result<Action, NotReady> {
        let contents = Contents::new(std::iter::empty());
        self.state.next(&NightQuery {
            now,
            contents: &contents,
            unavailable: &BTreeMap::new(),
        })
    }

    /// Appends what a `Decide` action says to commit.
    pub(crate) fn commit(&mut self, action: Action) -> Seq {
        let Action::Decide(decision) = action else {
            panic!("expected a decision, got {action:?}");
        };
        self.append(decision.payload.into(), &decision.causes)
    }
}

/// The backend build every trial runs unless a test rebuilds it.
pub(crate) fn content() -> ContentId {
    ContentId {
        backend: Backend::Mprime,
        executable: Digest::from_bytes([1; 32]),
        configuration: Digest::from_bytes([2; 32]),
    }
}

/// A rebuild of every backend.
pub(crate) fn rebuilt() -> ContentId {
    ContentId {
        backend: Backend::Mprime,
        executable: Digest::from_bytes([9; 32]),
        configuration: Digest::from_bytes([2; 32]),
    }
}

/// Every workload of every regime, as one build of the backends.
pub(crate) fn contents() -> Contents {
    contents_of(&content())
}

/// Every workload's content when each runs from `built`.
fn contents_of(built: &ContentId) -> Contents {
    Contents::new(togi_machine::ALL_REGIMES.into_iter().flat_map(|regime| {
        togi_machine::workloads(regime)
            .iter()
            .map(|w| (w.id(), built.clone()))
    }))
}

/// What a driven trial ends as.
#[derive(Clone, Copy)]
pub(crate) enum Ending {
    Pass,
    Interrupted,
    /// A conclusive failure, on `core` when it names one.
    Fail(Option<u8>),
}

impl Journal {
    /// Journals one trial of `plan`: intent, start and an end as `ending` says, as long as intended.
    pub(crate) fn run(&mut self, plan: &TrialPlan, ending: Ending) -> Seq {
        let built = self.built.clone();
        self.run_built(plan, ending, &built)
    }

    /// Journals one trial of `plan` whose every workload ran the backend build `built`.
    pub(crate) fn run_built(&mut self, plan: &TrialPlan, ending: Ending, built: &ContentId) -> Seq {
        self.run_from(plan, ending, built, None, &PerCore::default())
    }

    /// Journals one passing trial of `plan` whose end measured each loaded core's voltage request in
    /// `requests`, as a full-CCD R7 load reports them.
    pub(crate) fn run_requesting(&mut self, plan: &TrialPlan, requests: &PerCore<f64>) -> Seq {
        let built = self.built.clone();
        self.run_from(plan, Ending::Pass, &built, None, requests)
    }

    /// Journals one passing trial of `plan` whose task began `waited` seconds before its intent, as when the
    /// session prepared the profile and waited for a backend first.
    pub(crate) fn run_after_wait(&mut self, plan: &TrialPlan, waited: i64) -> Seq {
        let intent_at = at(i64::try_from(self.events.len()).unwrap() + 1);
        let task_start = intent_at
            .checked_sub(jiff::SignedDuration::from_secs(waited))
            .unwrap();
        self.run_from(
            plan,
            Ending::Pass,
            &content(),
            Some(task_start),
            &PerCore::default(),
        )
    }

    fn run_from(
        &mut self,
        plan: &TrialPlan,
        ending: Ending,
        built: &ContentId,
        task_start: Option<Timestamp>,
        requests: &PerCore<f64>,
    ) -> Seq {
        let id = TrialId::from(format!("t{}", self.events.len() + 1));
        let content = plan
            .workloads()
            .into_iter()
            .map(|workload| WorkloadContent {
                workload,
                content: built.clone(),
            })
            .collect();
        let cores = self.machine.clone();
        let mut intent = plan.intent(id.clone(), &cores, content).unwrap();
        intent.task_start = task_start;
        let duration_s = intent.duration_s;
        let intent = self.append(intent.into(), &plan.causes);
        let mut start: TrialStart = serde_json::from_value(serde_json::json!({
            "trial": id.as_str(), "scope": "scope", "pid": 1, "cpus": [], "argv": []
        }))
        .unwrap();
        start.roles = plan.roles(1, &cores).unwrap();
        self.append(start.into(), &[intent]);
        let (outcome, interrupted, core) = match ending {
            Ending::Pass => ("pass", false, None),
            Ending::Interrupted => ("failure", true, None),
            Ending::Fail(core) => ("failure", false, core),
        };
        let mut end = serde_json::json!({
            "trial": id.as_str(), "outcome": outcome, "duration_s": duration_s,
            "interrupted": interrupted
        });
        if let Some(core) = core {
            end["core"] = serde_json::json!(core);
        }
        let mut end: TrialEnd = serde_json::from_value(end).unwrap();
        end.voltage_requests_v = *requests;
        self.append(end.into(), &[intent])
    }

    /// Journals a trial of `plan` the session was killed in after its intent and before its start: the
    /// resumed run ends it inconclusive and interrupted, having run nothing.
    pub(crate) fn run_unstarted(&mut self, plan: &TrialPlan) -> Seq {
        let id = TrialId::from(format!("t{}", self.events.len() + 1));
        let content = plan
            .workloads()
            .into_iter()
            .map(|workload| WorkloadContent {
                workload,
                content: self.built.clone(),
            })
            .collect();
        let intent = plan.intent(id.clone(), &self.machine, content).unwrap();
        let intent = self.append(intent.into(), &plan.causes);
        let end: TrialEnd = serde_json::from_value(serde_json::json!({
            "trial": id.as_str(), "outcome": "inconclusive", "duration_s": 0, "interrupted": true
        }))
        .unwrap();
        self.append(end.into(), &[intent])
    }

    /// Drives the night until it asks for a trial, committing what it owes on the way.
    pub(crate) fn until_trial(&mut self, now: Timestamp) -> TrialPlan {
        match self.until_end(now, &BTreeMap::new()) {
            Ok(plan) => plan,
            Err(reason) => panic!("the night ended: {reason:?}"),
        }
    }

    /// Drives the night, each trial ending as `ending` says, until it asks for its first filler trial.
    pub(crate) fn until_filler(&mut self, ending: impl Fn(&TrialPlan) -> Ending) -> TrialPlan {
        for step in 0..3_500_i64 {
            let plan = match self.until_end(at(100 + step), &BTreeMap::new()) {
                Ok(plan) => plan,
                Err(reason) => panic!("the night ended at trial {step}: {reason:?}"),
            };
            if plan.task.queue == togi_journal::Queue::Filler {
                return plan;
            }
            self.run(&plan, ending(&plan));
        }
        panic!("the filler never ran");
    }

    /// Drives the night with `unavailable` excluded until it asks for a trial or ends.
    pub(crate) fn until_end(
        &mut self,
        now: Timestamp,
        unavailable: &BTreeMap<Backend, Vec<Seq>>,
    ) -> Result<TrialPlan, EndReason> {
        let contents = contents_of(&self.built);
        for _ in 0..200 {
            let action = self
                .state
                .next(&NightQuery {
                    now,
                    contents: &contents,
                    unavailable,
                })
                .unwrap();
            match action {
                Action::Decide(_) => {
                    self.commit(action);
                }
                Action::RunTrial(plan) => return Ok(plan),
                Action::EndNight(reason) => return Err(reason),
            }
        }
        panic!("the night kept deciding");
    }
}

/// The R1 class on `core` that `placement` charges, as one build of the backends runs it.
fn r1_class(core: u8, placement: Placement) -> ExposureClass {
    ExposureClass {
        regime: Regime::R1,
        workloads: vec![WorkloadContent {
            workload: r1_workload(),
            content: content(),
        }],
        cores: [CoreId::new(i64::from(core)).unwrap()]
            .into_iter()
            .collect(),
        position: None,
        duration_s: SEARCH_DURATION.get(),
        placement,
    }
}

fn r1_workload() -> WorkloadId {
    togi_machine::workloads(Regime::R1)[0].id()
}

/// `ccd`'s cap S at `offset` under its half of the machine budget and its committed `edge`, bound by the
/// rate of its first core's isolated R1 class.
fn cap(ccd_id: u8, offset: i64, edge: Edge) -> CcdCap {
    let class = r1_class(2 * ccd_id, Placement::Isolated);
    let offset = TargetOffset::new(offset).unwrap();
    CcdCap {
        ccd: ccd(ccd_id),
        old: TargetOffset::new(0).unwrap(),
        new: offset,
        budget: Rate::new(Params::SPEC.machine_budget.get() / 2.0).unwrap(),
        edge,
        adequate: vec![class.clone()],
        uncovered: Vec::new(),
        cells: Vec::new(),
        slope: Slope::Fallback {
            beta: Finite::new(-0.1).unwrap(),
        },
        candidates: vec![Candidate {
            class: class.clone(),
            offset,
        }],
        binding: Binding::Rate { class },
    }
}

/// The night's one R1 trial on `core` under `profile`, as `task` runs it.
fn r1_trial(
    night: togi_journal::NightId,
    task: TaskId,
    core: u8,
    profile: CcdProfile,
    tested: Tested,
    causes: Vec<Seq>,
) -> TrialPlan {
    let core = CoreId::new(i64::from(core)).unwrap();
    let (activity, purpose) = match tested {
        Tested::Isolated { .. } => (Activity::Mapping, Purpose::Isolated),
        Tested::Exact {} => (Activity::Qualification, Purpose::Exact),
    };
    TrialPlan {
        night,
        task,
        activity,
        cell: Cell {
            workloads: Workloads::One {
                workload: r1_workload(),
            },
            loaded: [core].into_iter().collect(),
            roles: Roles::SingleCore { core },
            duration: SEARCH_DURATION,
            purpose,
            sustained: false,
        },
        profile: togi_tuner::UnitProfile::of_ccd(&profile),
        tested,
        retry: false,
        causes,
    }
}

fn vector(profile: &CcdProfile) -> CcdVector {
    CcdVector::expand(profile.clone(), &cores()).unwrap()
}

impl Journal {
    /// Runs `plan` to a pass and observes its exposure of `class`; returns the trial's end and the exposure.
    fn witnessed_pass(&mut self, plan: &TrialPlan, class: &ExposureClass) -> (Seq, Seq) {
        let ended = self.run(plan, Ending::Pass);
        let Payload::TrialEnd(end) = &self.events.last().unwrap().payload else {
            unreachable!("a trial's last event is its end");
        };
        let trial = end.trial.clone();
        let observed = self.append(
            ExposureObserved {
                source: OriginalSource {
                    session: SESSION.parse().unwrap(),
                    seq: ended,
                    boot: BootId::from("tuning1".to_owned()),
                    trial: Some(trial),
                },
                origin: ExposureOrigin::Live {},
                firmware: context().bios_context(),
                epoch: togi_tuner::EVIDENCE_EPOCH,
                class: class.clone(),
                profile: vector(&plan.profile.to_ccd().unwrap()),
                tested: plan.tested,
                activity: plan.activity,
                outcome: Outcome::Pass,
                failures: Vec::new(),
                eligible_s: u64::from(SEARCH_DURATION.get()),
                uninterrupted_s: u64::from(SEARCH_DURATION.get()),
                exclusions: Vec::new(),
            }
            .into(),
            &[ended],
        );
        (ended, observed)
    }

    /// A promoted day built from public events alone, without running the night policy. The first night
    /// opens with the plan its first query commits. On each CCD, one isolated pass at the write limit
    /// censors its edge and a cap S of -1 follows. One exact pass then qualifies the proposal
    /// P = (CCD 0: -1, CCD 1: 0). The cold boot `day1` promotes P and its application commits it as D.
    ///
    /// For focused day tests; `crate::flow::promoted` reaches D through the whole first night instead.
    pub(crate) fn promoted_compact() -> (Self, CcdProfile) {
        let mut journal = Self::ready();
        journal.reboot("tuning1");
        let opening = journal.next(at(100)).unwrap();
        let night = togi_journal::NightId(journal.commit(opening));
        let limit = write_limit();
        for ccd_id in [0, 1] {
            let task = TaskId {
                queue: togi_journal::Queue::Mapping,
                ordinal: u32::from(ccd_id),
            };
            let tested = Tested::Isolated {
                ccd: ccd(ccd_id),
                offset: limit,
            };
            let plan = r1_trial(
                night,
                task,
                2 * ccd_id,
                zero().with(ccd(ccd_id), limit),
                tested,
                Vec::new(),
            );
            let (ended, _) =
                journal.witnessed_pass(&plan, &r1_class(2 * ccd_id, Placement::Isolated));
            let edge = Edge::Censored {
                limit,
                witnesses: vec![ended],
            };
            let censored = journal.append(
                CcdEdge {
                    ccd: ccd(ccd_id),
                    edge: edge.clone(),
                    old: Edge::Unknown {},
                    reason: format!("no failure down to the write limit {}", limit.get()),
                }
                .into(),
                &[ended],
            );
            journal.append(cap(ccd_id, -1, edge).into(), &[censored]);
        }
        let proposal = zero().with(ccd(0), TargetOffset::new(-1).unwrap());
        let cell = r1_class(0, Placement::Exact);
        let qualification = |state| NightQualification {
            profile: vector(&proposal),
            firmware: context().bios_context(),
            epoch: togi_tuner::EVIDENCE_EPOCH,
            contents: cell.workloads.clone(),
            matrix: Digest::from_bytes([3; 32]),
            purpose: QualificationPurpose::NextStep,
            state,
        };
        let started = journal.append(
            qualification(QualificationState::Start {
                outstanding: vec![cell.clone()],
            })
            .into(),
            &[],
        );
        let task = TaskId {
            queue: togi_journal::Queue::Qualification,
            ordinal: 0,
        };
        let plan = r1_trial(
            night,
            task,
            0,
            proposal.clone(),
            Tested::Exact {},
            vec![started],
        );
        let (_, observed) = journal.witnessed_pass(&plan, &cell);
        journal.append(
            qualification(QualificationState::Complete {
                witnesses: vec![CellWitness {
                    cell: cell.clone(),
                    evidence: CellEvidence::Fresh { exposure: observed },
                }],
            })
            .into(),
            &[observed],
        );
        journal.boot_start("day1");
        let committed = journal.settle_day(&cold_boot("day1", 60_000));
        let step = steps(&committed)
            .into_iter()
            .find(|step| matches!(step.step, StepKind::Promote { .. }))
            .cloned()
            .unwrap_or_else(|| panic!("no promotion in {committed:?}"));
        assert_eq!(step.proposed.ccd(), &proposal);
        apply(&mut journal, &step, 1, "day1");
        (journal, proposal)
    }
}
