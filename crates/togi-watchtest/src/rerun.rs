use std::collections::{BTreeMap, HashMap};

use jiff::Timestamp;
use togi_config::{Config, DEFAULT_PATH};
use togi_journal::{CheckingCycle, CorePhase, CycleEvent, Seq};
use togi_journal::{
    ConfigLoaded, CoreTransition, Event, Outcome, Payload, SessionStart, Stamp, TrialEnd, TrialId,
    TrialStart,
};
use togi_machine::{
    BootId, CcdId, CoreId, CoreInfo, CoreSet, CpuId, PerCore, Regime, Signal, TargetOffset,
};
use togi_tuner::{Action, State, TrialContext, TrialPlan};

use crate::Error;
use crate::simulate::build;

const ATTEMPTS: usize = 200;

/// The journals of a two-core session on cycle `[R1, R6]` where R1 passes, R6 fails naming core 00, and the reruns
/// the backoff owes pass, which completes cycle 1 before cycle 2's first trial.
#[derive(Clone, Debug)]
pub struct CycleCompletedByRerun {
    /// The journal as it stands while each rerun runs, after its `trial.start`.
    pub running: Vec<Vec<Event>>,
    /// The journal after the last rerun's end.
    pub passed: Vec<Event>,
    /// The journal after the cycle's end.
    pub ended: Vec<Event>,
}

/// Builds the session by asking the real tuner for each next step and answering it, with no simulator.
pub fn cycle_completed_by_rerun() -> Result<CycleCompletedByRerun, Error> {
    Fixture::new()?.run()
}

fn fixture(message: impl Into<String>) -> Error {
    Error::Fixture(message.into())
}

fn core(id: i64) -> Result<CoreId, Error> {
    Ok(CoreId::new(id)?)
}

fn offset(value: i64) -> Result<TargetOffset, Error> {
    Ok(TargetOffset::new(value)?)
}

struct Fixture {
    state: State,
    events: Vec<Event>,
    trials: u32,
    conclusive: HashMap<(Option<CoreId>, Regime), usize>,
}

impl Fixture {
    fn new() -> Result<Self, Error> {
        let mut fixture = Self {
            state: State::new(),
            events: Vec::new(),
            trials: 0,
            conclusive: HashMap::new(),
        };
        let cores = vec![
            CoreInfo {
                core: core(0)?,
                ccd: CcdId::new(0)?,
                cpus: vec![CpuId::from(0), CpuId::from(2)],
            },
            CoreInfo {
                core: core(1)?,
                ccd: CcdId::new(1)?,
                cpus: vec![CpuId::from(1), CpuId::from(3)],
            },
        ];
        let begin = fixture.add(
            SessionStart {
                build: build(),
                session: "20231114T221320Z".parse::<togi_journal::SessionId>()?,
                cores: cores.clone(),
            }
            .into(),
            &[],
        )?;
        let config = Config::parse(r#"{"checking":{"cycle":["R1","R6"]}}"#)?;
        fixture.add(
            ConfigLoaded {
                build: build(),
                config: config.snapshot(),
                path: DEFAULT_PATH.to_owned(),
                file: false,
                kernel_cursor: None,
                kernel_error: None,
            }
            .into(),
            &[],
        )?;
        for info in &cores {
            fixture.add(
                CoreTransition {
                    core: info.core,
                    from: None,
                    to: CorePhase::AtLimit,
                    offset: offset(-20)?,
                    pass: Some(offset(-20)?),
                    failure_point: Some(offset(-21)?),
                    check_solo_limit: false,
                    workloads: Vec::new(),
                    cleared_combination: Vec::new(),
                    reason: "test".to_owned(),
                }
                .into(),
                &[begin],
            )?;
        }
        Ok(fixture)
    }

    fn add(&mut self, payload: Payload, cause: &[Seq]) -> Result<Seq, Error> {
        let seq = Seq::new(self.events.len() as u64 + 1)
            .ok_or_else(|| fixture("the journal has no seq left"))?;
        let at = i64::try_from(self.events.len())?;
        let time = Timestamp::from_second(1000 + at)?;
        let event = Event::new(
            seq,
            Stamp { time, mono: None },
            BootId::from("boot".to_owned()),
            cause.to_vec(),
            payload,
        );
        self.state.fold(&event)?;
        self.events.push(event);
        Ok(seq)
    }

    fn trial(&mut self, plan: &TrialPlan) -> Result<(Seq, TrialId), Error> {
        self.trials += 1;
        let id = TrialId::from(format!("{:04}", self.trials));
        let owner = match plan.load {
            togi_tuner::Load::Single(core) => Some(core),
            togi_tuner::Load::Many(_) => None,
        };
        let rotation = self
            .conclusive
            .get(&(owner, plan.regime))
            .copied()
            .unwrap_or(0);
        let intent = self.add(plan.intent(id.clone(), rotation).into(), &plan.cause)?;
        self.add(
            TrialStart {
                trial: id.clone(),
                window_start_ns: None,
                scope: String::new(),
                pid: 0,
                cpus: Vec::new(),
                argv: Vec::new(),
                files: Vec::new(),
                instances: Vec::new(),
            }
            .into(),
            &[],
        )?;
        Ok((intent, id))
    }

    fn end(
        &mut self,
        plan: &TrialPlan,
        (intent, trial): (Seq, TrialId),
        outcome: Outcome,
        failed: Option<(Signal, CoreId, u32)>,
    ) -> Result<(), Error> {
        let (signal, named, duration_s) = match failed {
            Some((signal, core, duration)) => (Some(signal), Some(core), duration),
            None => (None, None, plan.duration.get()),
        };
        self.add(
            TrialEnd {
                trial,
                kernel_cursor: None,
                kernel_error: None,
                outcome,
                signal,
                core: named,
                duration_s,
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
            .into(),
            &[intent],
        )?;
        if outcome == Outcome::Pass {
            let owner = match plan.load {
                togi_tuner::Load::Single(core) => Some(core),
                togi_tuner::Load::Many(_) => None,
            };
            *self.conclusive.entry((owner, plan.regime)).or_insert(0) += 1;
        }
        Ok(())
    }

    fn run(mut self) -> Result<CycleCompletedByRerun, Error> {
        let mut running = Vec::new();
        let mut passed = None;
        let mut ended = None;
        let mut failed_r6 = false;
        for _ in 0..ATTEMPTS {
            let action = self.state.next()?;
            match action {
                Action::Decide(decision) => {
                    let ends_cycle_one = matches!(
                        &decision.payload,
                        togi_tuner::DecisionPayload::CheckingCycle(CheckingCycle {
                            event: CycleEvent::End,
                            cycle,
                            ..
                        }) if failed_r6 && cycle.get() == 1
                    );
                    let seq = self.add(decision.payload.into(), &decision.cause)?;
                    if ends_cycle_one {
                        let upto = usize::try_from(seq.get())?;
                        ended = Some(self.events[..upto].to_vec());
                    }
                }
                Action::ReadRanking => return Err(fixture("the tuner asked for a ranking read")),
                Action::RunTrial(plan) => match plan.requirement.context {
                    TrialContext::Rerun { .. } => {
                        let trial = self.trial(&plan)?;
                        running.push(self.events.clone());
                        self.end(&plan, trial, Outcome::Pass, None)?;
                        passed = Some(self.events.clone());
                    }
                    TrialContext::Cycle { cycle, .. } if cycle.get() == 2 => {
                        return match (passed, ended) {
                            (Some(passed), Some(ended)) if !running.is_empty() => {
                                Ok(CycleCompletedByRerun {
                                    running,
                                    passed,
                                    ended,
                                })
                            }
                            _ => Err(fixture("cycle 2 started without reruns completing cycle 1")),
                        };
                    }
                    _ if plan.regime == Regime::R6 && !failed_r6 => {
                        failed_r6 = true;
                        let trial = self.trial(&plan)?;
                        self.end(
                            &plan,
                            trial,
                            Outcome::Failure,
                            Some((Signal::ComputationError, core(0)?, 41)),
                        )?;
                    }
                    _ if failed_r6 => {
                        return Err(fixture(format!(
                            "cycle 1 ran {:?} after its R6 failure instead of completing on the reruns",
                            plan.requirement.context
                        )));
                    }
                    TrialContext::Search { .. }
                    | TrialContext::SoloLimit { .. }
                    | TrialContext::Cycle { .. }
                    | TrialContext::ZeroRerun { .. }
                    | TrialContext::Round { .. }
                    | TrialContext::Hunt { .. } => {
                        let trial = self.trial(&plan)?;
                        self.end(&plan, trial, Outcome::Pass, None)?;
                    }
                },
            }
        }
        Err(fixture("the session never reached cycle 2"))
    }
}
