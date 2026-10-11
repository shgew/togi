use std::collections::BTreeMap;

use jiff::Timestamp;
use togi_config::Config;
use togi_journal::{
    Attribution, Build, ConfigLoaded, CorePhase, CoreTransition, Event, Failure, Outcome, Payload,
    SCHEMA, Seq, SessionStart, Stamp, TrialEnd, TrialId, TunerDecision, TunerWarning,
};
use togi_machine::{
    BootId, CcdId, Condition, CoreId, CoreInfo, CoreSet, PerCore, Regime, Signal, TargetOffset,
    WorkloadId,
};

use crate::tests::{core, off};
use crate::{
    Action, ClassKey, DecisionAction, FoldError, Load, Profile, Requirement, RequirementKind, Secs,
    State, TrialContext, TrialPlan, Window, WorkloadChoice,
};

pub(crate) fn build() -> Build {
    Build {
        version: "0.1.0".into(),
        rev: None,
        ruleset: 1,
        schema: SCHEMA,
        fixes: 0,
        evidence: 1,
    }
}

pub(crate) fn config(json: &str) -> Config {
    Config::parse(json).unwrap()
}

pub(crate) const MPRIME_OLD: &str = "/nix/store/64hjzgj1msiyndpdxrk9l3gkjf3sczgj-mprime-31.04b02";
pub(crate) const MPRIME_NEW: &str = "/nix/store/0000000000000000000000000000000a-mprime-31.04b03";
pub(crate) const YCRUNCHER_OLD: &str =
    "/nix/store/n5g91xa9pzcfqyyh62xpwz3v53f87y0a-y-cruncher-0.8.7.9547";
pub(crate) const YCRUNCHER_NEW: &str =
    "/nix/store/0000000000000000000000000000000b-y-cruncher-0.8.7.9548";

pub(crate) fn backends(mprime: &str, ycruncher: &str) -> Config {
    config(&format!(
        r#"{{"backends":{{"mprime":"{mprime}","ycruncher":"{ycruncher}"}}}}"#
    ))
}

pub(crate) fn infos(ids: &[u8]) -> Vec<CoreInfo> {
    ids.iter()
        .map(|&id| CoreInfo {
            core: core(id),
            ccd: CcdId(id / 8),
            cpus: Vec::new(),
        })
        .collect()
}

pub(crate) fn set(ids: &[u8]) -> CoreSet {
    ids.iter().map(|&id| core(id)).collect()
}

/// How a core stands when a test's session starts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Start {
    pub(crate) phase: CorePhase,
    pub(crate) from: Option<CorePhase>,
    pub(crate) offset: i64,
    pub(crate) pass: Option<i64>,
    pub(crate) fail: Option<i64>,
    pub(crate) check: bool,
}

impl Start {
    pub(crate) fn search(offset: i64) -> Self {
        Self {
            phase: CorePhase::Search,
            from: None,
            offset,
            pass: None,
            fail: None,
            check: false,
        }
    }

    pub(crate) fn at_limit(offset: i64) -> Self {
        Self {
            phase: CorePhase::AtLimit,
            ..Self::search(offset)
        }
    }

    pub(crate) fn has_room(offset: i64) -> Self {
        Self {
            phase: CorePhase::HasRoom,
            ..Self::search(offset)
        }
    }

    pub(crate) fn fail(self, offset: i64) -> Self {
        Self {
            fail: Some(offset),
            ..self
        }
    }

    pub(crate) fn pass(self, offset: i64) -> Self {
        Self {
            pass: Some(offset),
            ..self
        }
    }

    pub(crate) fn checking(self) -> Self {
        Self {
            check: true,
            ..self
        }
    }

    /// A core that left search at `offset` with a solo limit of `solo`.
    pub(crate) fn solo(offset: i64, solo: i64) -> Self {
        let start = Self {
            from: Some(CorePhase::Search),
            ..Self::has_room(offset).pass(solo)
        };
        if solo > -50 {
            start.fail(solo - 1)
        } else {
            start
        }
    }
}

/// An event journal that folds as it grows, appending the way a session would.
pub(crate) struct Journal {
    pub(crate) events: Vec<Event>,
    pub(crate) state: State,
    ids: Vec<u8>,
    trials: u32,
    resume_stride: usize,
}

pub(crate) fn fresh(events: &[Event]) -> State {
    let mut state = State::new();
    for event in events {
        state.fold(event).unwrap();
    }
    state
}

impl Journal {
    /// A session over cores `ids` (two CCDs: ids below 8 and the rest), each core starting as
    /// `starts` says, in id order, under the default configuration.
    pub(crate) fn new(ids: &[u8], starts: &[Start]) -> Self {
        let mut journal = Self::bare(ids);
        let begin = journal.append(
            SessionStart {
                build: build(),
                session: "20231114T221320Z".parse().unwrap(),
                cores: infos(ids),
            }
            .into(),
            &[],
        );
        journal.load(&Config::default());
        for (&id, start) in ids.iter().zip(starts) {
            journal.start_core(id, *start, begin);
        }
        journal
    }

    pub(crate) fn search_at(offsets: &[i64]) -> Self {
        let ids: Vec<u8> = (0..offsets.len())
            .map(|id| u8::try_from(id).unwrap())
            .collect();
        let starts: Vec<Start> = offsets
            .iter()
            .map(|&offset| Start::search(offset))
            .collect();
        Self::new(&ids, &starts)
    }

    pub(crate) fn has_room(offsets: &[i64]) -> Self {
        let ids: Vec<u8> = (0..offsets.len())
            .map(|id| u8::try_from(id).unwrap())
            .collect();
        let starts: Vec<Start> = offsets
            .iter()
            .map(|&offset| Start::has_room(offset))
            .collect();
        Self::new(&ids, &starts)
    }

    pub(crate) fn bare(ids: &[u8]) -> Self {
        Self {
            events: Vec::new(),
            state: State::new(),
            ids: ids.to_vec(),
            trials: 0,
            resume_stride: 0,
        }
    }

    /// The journal as a resumed session holds it: the same events folded afresh.
    pub(crate) fn resumed(&self) -> Self {
        Self {
            events: self.events.clone(),
            state: fresh(&self.events),
            ids: self.ids.clone(),
            trials: self.trials,
            resume_stride: 0,
        }
    }

    pub(crate) fn resuming_every(mut self, stride: usize) -> Self {
        self.resume_stride = stride;
        self
    }

    pub(crate) fn next_seq(&self) -> Seq {
        Seq::new(self.events.len() as u64 + 1).unwrap()
    }

    pub(crate) fn event(&self, payload: Payload, cause: &[Seq]) -> Event {
        let seq = self.next_seq();
        let time =
            Timestamp::from_second(1_700_000_000 + i64::try_from(seq.get()).unwrap()).unwrap();
        Event::new(
            seq,
            Stamp { time, mono: None },
            BootId::from("boot".to_owned()),
            cause.to_vec(),
            payload,
        )
    }

    pub(crate) fn try_append(&mut self, payload: Payload, cause: &[Seq]) -> Result<Seq, FoldError> {
        let event = self.event(payload, cause);
        self.state.fold(&event)?;
        let seq = event.seq;
        self.events.push(event);
        if self.resume_stride > 0 && self.events.len().is_multiple_of(self.resume_stride) {
            self.assert_resumes();
        }
        Ok(seq)
    }

    pub(crate) fn append(&mut self, payload: Payload, cause: &[Seq]) -> Seq {
        self.try_append(payload, cause).unwrap()
    }

    pub(crate) fn assert_resumes(&self) {
        assert_eq!(
            fresh(&self.events),
            self.state,
            "a fresh fold differs from the incremental one"
        );
    }

    pub(crate) fn load(&mut self, config: &Config) -> Seq {
        self.append(
            ConfigLoaded {
                build: build(),
                config: config.clone(),
                path: "/etc/togi/config.json".to_owned(),
                file: false,
                kernel_cursor: None,
                kernel_error: None,
            }
            .into(),
            &[],
        )
    }

    pub(crate) fn baseline(&mut self, values: &[(u8, i64)]) -> Seq {
        self.append(
            togi_journal::SessionBaseline {
                offsets: values
                    .iter()
                    .map(|&(id, value)| (core(id), togi_machine::ObservedOffset(value)))
                    .collect(),
            }
            .into(),
            &[],
        )
    }

    fn start_core(&mut self, id: u8, start: Start, begin: Seq) -> Seq {
        let workloads = if start.check {
            vec![
                togi_machine::workloads(Regime::R1)[0].id(),
                togi_machine::workloads(Regime::R2)[0].id(),
            ]
        } else {
            Vec::new()
        };
        self.append(
            CoreTransition {
                core: core(id),
                from: start.from,
                to: start.phase,
                offset: off(start.offset),
                pass: start.pass.map(off),
                failure_point: start.fail.map(off),
                check_solo_limit: start.check,
                workloads,
                cleared_combination: Vec::new(),
                reason: "test".to_owned(),
            }
            .into(),
            &[begin],
        )
    }

    pub(crate) fn core_phase(&mut self, transition: CoreTransition) -> Seq {
        self.append(transition.into(), &[])
    }

    pub(crate) fn decision(&mut self, decision: TunerDecision) -> Seq {
        self.append(decision.into(), &[])
    }

    /// Appends the decision as a session does: its payload, citing its cause.
    pub(crate) fn decide(&mut self, action: DecisionAction) -> Seq {
        self.append(action.payload.into(), &action.cause)
    }

    pub(crate) fn rotation(&self, plan: &TrialPlan) -> usize {
        self.state.run().unwrap().rotation(plan)
    }

    pub(crate) fn intent(&mut self, plan: &TrialPlan) -> (Seq, TrialId) {
        self.trials += 1;
        let trial = TrialId::from(format!("{:04}", self.trials));
        let intent = plan.intent(trial.clone(), self.rotation(plan));
        let seq = self.append(intent.into(), &plan.cause);
        (seq, trial)
    }

    pub(crate) fn end_payload(
        trial: &TrialId,
        outcome: Outcome,
        signal: Option<Signal>,
    ) -> TrialEnd {
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

    pub(crate) fn end(&mut self, intent: Seq, trial: &TrialId, outcome: Outcome) -> Seq {
        let signal = (outcome == Outcome::Failure).then_some(Signal::ComputationError);
        self.append(Self::end_payload(trial, outcome, signal).into(), &[intent])
    }

    /// Runs `plan` to the end `outcome` and returns the end's seq.
    pub(crate) fn trial(&mut self, plan: &TrialPlan, outcome: Outcome) -> Seq {
        let (intent, trial) = self.intent(plan);
        self.end(intent, &trial, outcome)
    }

    /// The tuner's next action, which the state must have.
    pub(crate) fn next(&self) -> Action {
        self.state.decide().expect("the tuner has a next action")
    }

    pub(crate) fn next_trial(&self) -> TrialPlan {
        match self.next() {
            Action::RunTrial(plan) => plan,
            other => panic!("expected a trial, the tuner asked for {other:?}"),
        }
    }

    pub(crate) fn next_decision(&self) -> DecisionAction {
        match self.next() {
            Action::Decide(decision) => decision,
            other => panic!("expected a decision, the tuner asked for {other:?}"),
        }
    }

    /// Performs the tuner's next action; trials end as `oracle` says.
    pub(crate) fn step(&mut self, oracle: &mut impl FnMut(&TrialPlan) -> Outcome) -> Action {
        let action = self.next();
        match &action {
            Action::Decide(decision) => {
                self.decide(decision.clone());
            }
            Action::RunTrial(plan) => {
                let outcome = oracle(plan);
                self.trial(plan, outcome);
            }
            Action::ReadRanking => panic!("the tuner asked to read a ranking"),
        }
        action
    }

    /// Steps until `stop` holds of the action just performed or `limit` actions ran.
    pub(crate) fn run_until(
        &mut self,
        limit: usize,
        mut oracle: impl FnMut(&TrialPlan) -> Outcome,
        mut stop: impl FnMut(&Action) -> bool,
    ) -> Vec<Action> {
        let mut done = Vec::new();
        for _ in 0..limit {
            let action = self.step(&mut oracle);
            let finished = stop(&action);
            done.push(action);
            if finished {
                break;
            }
        }
        done
    }

    pub(crate) fn failure(&mut self, failure: Failure, cause: &[Seq]) -> Seq {
        self.append(failure.into(), cause)
    }

    pub(crate) fn idle_failure(
        &mut self,
        profile: PerCore<TargetOffset>,
        condition: Condition,
    ) -> Seq {
        self.failure(
            Failure {
                signal: Signal::Crash,
                attribution: Attribution::Unattributed,
                core: None,
                offset: None,
                trial: None,
                regime: Some(Regime::R6),
                condition: Some(condition),
                profile: Some(profile),
                known_failure: None,
                reason: None,
                round: None,
            },
            &[],
        )
    }

    /// The profile with `values` at the session's cores in id order.
    pub(crate) fn profile(&self, values: &[i64]) -> Profile {
        let offsets: PerCore<TargetOffset> = self
            .ids
            .iter()
            .zip(values)
            .map(|(&id, &value)| (core(id), off(value)))
            .collect();
        Profile::new(self.state.run().unwrap().topo_ref(), offsets).unwrap()
    }

    /// A trial the tuner did not plan, for tests of evidence: a checking trial of `workload`.
    pub(crate) fn manual(
        &self,
        regime: Regime,
        workload: usize,
        load: Load,
        condition: Condition,
        profile: &[i64],
        duration: u32,
    ) -> TrialPlan {
        let id = WorkloadId::new(regime, workload).unwrap();
        let duration = Secs::new(std::num::NonZeroU32::new(duration).unwrap());
        TrialPlan {
            regime,
            workload: WorkloadChoice::Frozen(id),
            load,
            condition,
            activity: togi_journal::Activity::Checking,
            profile: self.profile(profile),
            duration,
            context: TrialContext::SoloLimit {
                core: core(self.ids[0]),
                regime,
            },
            requirement: Requirement {
                kind: RequirementKind::Cycle,
                class: ClassKey::new(id, load.cores(), duration),
                window: Window::Live { since: None },
                needed: std::num::NonZeroU16::MIN,
                step: None,
                part: None,
            },
            retry: false,
            cause: Vec::new(),
        }
    }

    /// A fact carried from `session`: a trial of the class that ended `outcome` at `profile`.
    pub(crate) fn carry(
        &mut self,
        session: &str,
        class: (Regime, usize, &[u8], u32),
        condition: Condition,
        profile: &[i64],
        outcome: Outcome,
    ) -> Seq {
        self.carry_under(session, class, condition, profile, outcome, None)
    }

    /// As `carry`, for a trial that ran from the backend package at `backend`.
    pub(crate) fn carry_under(
        &mut self,
        session: &str,
        class: (Regime, usize, &[u8], u32),
        condition: Condition,
        profile: &[i64],
        outcome: Outcome,
        backend: Option<(togi_machine::Backend, &str)>,
    ) -> Seq {
        let (regime, workload, cores, duration) = class;
        let seq = self.next_seq();
        let signal = (outcome == Outcome::Failure).then_some(Signal::Crash);
        let payload = togi_journal::TrialCarried {
            source: togi_journal::FactSource {
                session: session.parse().unwrap(),
                seq,
                build: build(),
                trial: None,
                time: Timestamp::from_second(1_600_000_000).unwrap(),
                boot: BootId::from("old".to_owned()),
                backend: backend.map(|(backend, path)| togi_journal::BackendIdentity {
                    backend,
                    path: path.to_owned(),
                }),
            },
            class: togi_journal::TrialClass {
                regime,
                workload: WorkloadId::new(regime, workload),
                cores: set(cores),
                duration_s: duration,
            },
            condition,
            activity: None,
            rerun: false,
            profile: self.profile(profile).offsets(),
            outcome,
            signal,
            duration_s: duration,
            core: None,
            stalled_core: None,
            voltage_requests_v: PerCore::default(),
            top_requesters: CoreSet::default(),
            ccd_mhz: BTreeMap::new(),
        };
        self.append(payload.into(), &[])
    }

    /// A failure carried from `session` that no trial explains: an idle crash at `profile`.
    pub(crate) fn carry_idle(&mut self, session: &str, profile: &[i64]) -> Seq {
        let seq = self.next_seq();
        let payload = togi_journal::FailureCarried {
            source: togi_journal::FactSource {
                session: session.parse().unwrap(),
                seq,
                build: build(),
                trial: None,
                time: Timestamp::from_second(1_600_000_000).unwrap(),
                boot: BootId::from("old".to_owned()),
                backend: None,
            },
            class: togi_journal::TrialClass {
                regime: Regime::R6,
                workload: None,
                cores: self.run().topo_ref().all(),
                duration_s: 900,
            },
            failure: Failure {
                signal: Signal::Crash,
                attribution: Attribution::Unattributed,
                core: None,
                offset: None,
                trial: None,
                regime: Some(Regime::R6),
                condition: Some(Condition::Together),
                profile: Some(self.profile(profile).offsets()),
                known_failure: None,
                reason: None,
                round: None,
            },
        };
        self.append(payload.into(), &[])
    }

    pub(crate) fn profile_change(&mut self, values: &[i64]) -> Seq {
        let to = self.profile(values).offsets();
        self.append(togi_journal::ProfileChange { from: None, to }.into(), &[])
    }

    pub(crate) fn cycle_start(&mut self, cycle: u32, steps: &[Regime]) -> Seq {
        self.append(
            togi_journal::CheckingCycle {
                cycle: std::num::NonZeroU32::new(cycle).unwrap(),
                event: togi_journal::CycleEvent::Start,
                passed: false,
                full: false,
                missing: Vec::new(),
                steps: steps.to_vec(),
                reason: None,
            }
            .into(),
            &[],
        )
    }

    pub(crate) fn cycle_end(&mut self, cycle: u32, passed: bool, full: bool) -> Seq {
        self.append(
            togi_journal::CheckingCycle {
                cycle: std::num::NonZeroU32::new(cycle).unwrap(),
                event: togi_journal::CycleEvent::End,
                passed,
                full,
                missing: Vec::new(),
                steps: Vec::new(),
                reason: None,
            }
            .into(),
            &[],
        )
    }

    pub(crate) fn combination(&mut self, id: u32, members: &[(u8, i64)]) -> Seq {
        self.append(
            togi_journal::Combination {
                combination: std::num::NonZeroU32::new(id).unwrap(),
                members: members
                    .iter()
                    .map(|&(core_id, offset)| togi_journal::Member {
                        core: core(core_id),
                        offset: off(offset),
                    })
                    .collect(),
                fallback: false,
                hunt: std::num::NonZeroU32::MIN,
                reason: "test".to_owned(),
            }
            .into(),
            &[],
        )
    }

    pub(crate) fn reset(&mut self, id: u8) -> Seq {
        self.append(
            togi_journal::CommandReset {
                target: togi_journal::ResetTarget::Core(core(id)),
            }
            .into(),
            &[],
        )
    }

    pub(crate) fn run(&self) -> &crate::state::Run {
        self.state.run().unwrap()
    }

    pub(crate) fn core_id(&self, index: usize) -> CoreId {
        core(self.ids[index])
    }

    pub(crate) fn warning(&self) -> Option<TunerWarning> {
        match self.state.decide() {
            Some(Action::Decide(DecisionAction {
                payload: crate::DecisionPayload::TunerWarning(warning),
                ..
            })) => Some(warning),
            _ => None,
        }
    }
}
