use std::num::NonZeroU32;

use jiff::Timestamp;
use togi_journal::{
    Attribution, Event, HostRanking, Outcome, Payload, Stamp, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{BootId, Condition, CoreId, CoreSet, PerCore, Regime, Signal, TargetOffset};

use crate::cores::Phase;
use crate::flight::OpenTrial;
use crate::r7::order::RequestOrder;
use crate::state::Run;
use crate::{
    Action, DecisionPayload, Load, LoadKey, Profile, State, StepNo, TrialContext, TrialPlan,
    WorkloadChoice,
};

const STEPS: usize = 1000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Premise {
    Pass,
    AllPass,
    Named(CoreId),
    Unnamed,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Uncertainty {
    NeedsRanking,
    NeedsHistory,
    RecurringPlan,
    Stopped { after: u16 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AllPass {
    Complete,
    Diverged { after: u32 },
    Stopped(Uncertainty),
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts of one premise's outcome"
)]
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Branch {
    pub premise: Premise,
    pub passes: u32,
    pub all_pass: Option<AllPass>,
    pub decisions: Vec<DecisionPayload>,
    pub next: Option<TrialPlan>,
    pub next_step: Option<StepNo>,
    pub resume: Option<StepNo>,
    pub uncertainty: Option<Uncertainty>,
    pub without_telemetry: bool,
    pub offset_order: bool,
    pub at_zero: bool,
    pub top_requester: bool,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Between {
    pub decisions: Vec<DecisionPayload>,
    pub next: Option<TrialPlan>,
    pub next_step: Option<StepNo>,
    pub resume: Option<StepNo>,
    pub uncertainty: Option<Uncertainty>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Forecast {
    pub branches: Vec<Branch>,
    pub between: Option<Between>,
}

#[derive(Debug, Default)]
struct Settled {
    decisions: Vec<DecisionPayload>,
    next: Option<TrialPlan>,
    next_step: Option<StepNo>,
    uncertainty: Option<Uncertainty>,
}

#[derive(Clone)]
struct Sim {
    state: State,
    ranking: Option<Vec<CoreId>>,
}

fn after(decisions: usize) -> u16 {
    crate::numbers::saturating_forecast_count(decisions)
}

impl Sim {
    fn run(&self) -> Option<&Run> {
        self.state.run()
    }

    fn fold(&mut self, payload: Payload, cause: Vec<togi_journal::Seq>) -> bool {
        let Some(seq) = self.run().and_then(|run| run.last_seq.next()) else {
            return false;
        };
        let event = Event::new(
            seq,
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: None,
            },
            BootId::from("forecast".to_owned()),
            cause,
            payload,
        );
        self.state.fold(&event).is_ok()
    }

    fn resolved(&self, mut plan: TrialPlan) -> Option<TrialPlan> {
        let workload = plan.workload_at(self.run()?.rotation(&plan));
        plan.workload = WorkloadChoice::Frozen(workload);
        Some(plan)
    }

    fn settle(&mut self) -> Settled {
        let mut settled = Settled::default();
        let mut plans = Vec::new();
        for _ in 0..STEPS {
            let Some(run) = self.run() else {
                settled.uncertainty = Some(Uncertainty::NeedsHistory);
                return settled;
            };
            if run.hunt_settled() && run.checking.profile().is_none() {
                settled.uncertainty = Some(Uncertainty::NeedsHistory);
                return settled;
            }
            match self.state.next() {
                Err(_) => {
                    settled.uncertainty = Some(Uncertainty::Stopped {
                        after: after(settled.decisions.len()),
                    });
                    return settled;
                }
                Ok(Action::RunTrial(plan)) => {
                    settled.next_step =
                        if let TrialContext::Cycle { step, .. } = plan.requirement.context {
                            Some(step)
                        } else {
                            None
                        };
                    settled.next = self.resolved(plan);
                    return settled;
                }
                Ok(Action::ReadRanking) => {
                    let Some(order) = self.ranking.clone() else {
                        settled.uncertainty = Some(Uncertainty::NeedsRanking);
                        return settled;
                    };
                    let read = HostRanking {
                        ranking: order,
                        values: PerCore::default(),
                        detail: None,
                    };
                    if !self.fold(read.into(), Vec::new()) {
                        settled.uncertainty = Some(Uncertainty::NeedsHistory);
                        return settled;
                    }
                }
                Ok(Action::Decide(decision)) => {
                    if let DecisionPayload::HuntGroup(group) = &decision.payload {
                        if plans.iter().any(|prior| same_plan(prior, group)) {
                            settled.uncertainty = Some(Uncertainty::RecurringPlan);
                            return settled;
                        }
                        plans.push(group.clone());
                    }
                    let dead_end = matches!(decision.payload, DecisionPayload::DeadEnd(_));
                    settled.decisions.push(decision.payload.clone());
                    if !self.fold(decision.payload.into(), decision.cause) || dead_end {
                        if !dead_end {
                            settled.uncertainty = Some(Uncertainty::NeedsHistory);
                        }
                        return settled;
                    }
                }
            }
        }
        settled.uncertainty = Some(Uncertainty::NeedsHistory);
        settled
    }

    fn end(&mut self, intent: &TrialIntent, premise: Premise) -> bool {
        let mut end = TrialEnd {
            trial: intent.trial.clone(),
            kernel_cursor: None,
            kernel_error: None,
            outcome: Outcome::Pass,
            signal: None,
            core: None,
            duration_s: intent.duration_s,
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
        };
        match premise {
            Premise::Pass | Premise::AllPass => {}
            Premise::Named(core) => {
                end.outcome = Outcome::Failure;
                end.signal = Some(Signal::ComputationError);
                end.core = Some(core);
            }
            Premise::Unnamed => {
                end.outcome = Outcome::Failure;
                end.signal = Some(Signal::ComputationError);
            }
            Premise::Inconclusive => {
                end.outcome = Outcome::Inconclusive;
                end.duration_s = 0;
            }
        }
        let cause = self
            .run()
            .and_then(|run| run.flight.open())
            .map(|open| open.seq);
        self.fold(end.into(), cause.into_iter().collect())
    }

    fn run_trial(&mut self, plan: &TrialPlan) -> bool {
        let Some(run) = self.run() else {
            return false;
        };
        let Some(seq) = run.last_seq.next() else {
            return false;
        };
        let trial = TrialId::from(format!("forecast-{}", seq.get()));
        let intent = plan.intent(trial, run.rotation(plan));
        self.fold(intent.clone().into(), plan.cause.clone()) && self.end(&intent, Premise::Pass)
    }

    fn pass_remaining(
        &mut self,
        open: &OpenTrial,
        remaining: u32,
        settled: &mut Settled,
        passes: &mut u32,
    ) -> AllPass {
        for _ in 1..remaining {
            let Some(plan) = settled.next.clone() else {
                return AllPass::Stopped(settled.uncertainty.unwrap_or(Uncertainty::Stopped {
                    after: after(settled.decisions.len()),
                }));
            };
            if !repeats(open, &plan) {
                return AllPass::Diverged { after: *passes };
            }
            if !self.run_trial(&plan) {
                settled.next = None;
                settled.uncertainty = Some(Uncertainty::NeedsHistory);
                return AllPass::Stopped(Uncertainty::NeedsHistory);
            }
            *passes += 1;
            *settled = self.settle();
        }
        AllPass::Complete
    }

    fn resume(&mut self, first: &Settled, cycle: Option<NonZeroU32>) -> Option<StepNo> {
        let mut settled = Settled {
            next: first.next.clone(),
            uncertainty: first.uncertainty,
            ..Settled::default()
        };
        for _ in 0..STEPS {
            if settled.uncertainty.is_some() {
                return None;
            }
            let plan = settled.next.clone()?;
            match plan.requirement.context {
                TrialContext::Cycle {
                    cycle: number,
                    step,
                    ..
                } => {
                    return (Some(number) == cycle).then_some(step);
                }
                TrialContext::Rerun { .. } => {
                    if !self.run_trial(&plan) {
                        return None;
                    }
                    settled = self.settle();
                }
                TrialContext::Search { .. }
                | TrialContext::SoloLimit { .. }
                | TrialContext::ZeroRerun { .. }
                | TrialContext::Round { .. }
                | TrialContext::Hunt { .. } => return None,
            }
        }
        None
    }
}

fn same_plan(a: &togi_journal::HuntGroup, b: &togi_journal::HuntGroup) -> bool {
    a.hunt == b.hunt
        && a.duration_s == b.duration_s
        && a.granularity == b.granularity
        && a.stage == b.stage
        && a.index == b.index
        && a.escalated == b.escalated
        && a.full_checked == b.full_checked
        && a.any_failed == b.any_failed
        && a.inferred == b.inferred
        && a.skipped == b.skipped
        && a.probe == b.probe
        && a.cores == b.cores
        && a.set == b.set
        && a.profile == b.profile
        && a.held == b.held
}

fn repeats(open: &OpenTrial, plan: &TrialPlan) -> bool {
    let intent = &open.intent;
    let next = plan.intent(intent.trial.clone(), 0);
    plan.requirement == open.requirement
        && plan.profile == open.profile
        && next.activity == intent.activity
        && next.condition == intent.condition
        && next.rerun == intent.rerun
        && next.cycle == intent.cycle
        && next.hunt == intent.hunt
        && next.group == intent.group
        && next.round == intent.round
        && next.step == intent.step
}

fn multi_core_r7(intent: &TrialIntent) -> bool {
    intent.regime == Regime::R7 && intent.core.is_none() && intent.cores.len() > 1
}

impl Run {
    fn request_order(&self, intent: &TrialIntent, profile: &Profile) -> RequestOrder {
        RequestOrder::derive(
            &self.r7.measurements,
            &self.topo,
            LoadKey::new(
                intent.workload,
                Load::recorded(intent.core, intent.cores).cores(),
            ),
            profile,
        )
    }

    fn off_zero(profile: &Profile, core: CoreId) -> bool {
        profile
            .get(core)
            .is_none_or(|offset| offset == TargetOffset::ZERO)
    }

    fn named_representatives(&self, intent: &TrialIntent, profile: &Profile) -> Vec<CoreId> {
        let loaded = Load::recorded(intent.core, intent.cores).cores();
        if multi_core_r7(intent) {
            let top = self.request_order(intent, profile).top();
            let mut movable = None;
            let mut zero_top = None;
            let mut zero: Vec<CoreId> = Vec::new();
            let mut zero_ccds = Vec::new();
            for core in loaded {
                let ccd = self.topo.ccd_of(core);
                if !Self::off_zero(profile, core) {
                    movable.get_or_insert(core);
                } else if top.contains(core) {
                    zero_top.get_or_insert(core);
                } else if !zero_ccds.contains(&ccd) {
                    zero_ccds.push(ccd);
                    zero.push(core);
                }
            }
            return movable.into_iter().chain(zero).chain(zero_top).collect();
        }
        let mut cores = loaded;
        if let (Some(hunt), Some(active)) = (intent.hunt, self.hunts.active.as_ref()) {
            if hunt == active.id {
                if let Some(group) = active
                    .groups
                    .iter()
                    .find(|group| Some(group.payload.group) == intent.group)
                {
                    let probe = group.payload.probe.map(|member| member.core);
                    cores = cores
                        .iter()
                        .filter(|&core| group.payload.cores.contains(core) || probe == Some(core))
                        .collect();
                }
            }
        }
        if cores.is_empty() {
            cores = loaded;
        }
        let Some(first) = cores.iter().next() else {
            return Vec::new();
        };
        let stand_in = cores
            .iter()
            .find(|&core| !Self::off_zero(profile, core))
            .unwrap_or(first);
        let mut named = vec![stand_in];
        if let Some(other) = loaded
            .iter()
            .find(|&core| core != stand_in && Self::off_zero(profile, core))
        {
            named.push(other);
        }
        named
    }
}

impl State {
    fn simulation(&self) -> Sim {
        Sim {
            state: self.speculative_copy(),
            ranking: self.run().and_then(|run| {
                run.ranking
                    .as_ref()
                    .map(|ranked| ranked.ranking.order().to_vec())
            }),
        }
    }

    pub fn forecast(&self) -> Forecast {
        let Some(run) = self.run().filter(|_| !self.poisoned()) else {
            return Forecast::default();
        };
        if run.stopped || run.topo.order().is_empty() || run.next_unphased() {
            return Forecast::default();
        }
        let cycle = run.checking.last_cycle();
        let open = run.flight.open().filter(|open| !open.crashed);
        let Some(open) = open else {
            return Forecast {
                branches: Vec::new(),
                between: Some(self.between(cycle)),
            };
        };
        let history = run.history(open);
        let remaining = history.needed.saturating_sub(history.passed).max(1);
        let intent = &open.intent;
        let mut premises = vec![Premise::Pass];
        if remaining > 1 {
            premises.push(Premise::AllPass);
        }
        let multi = multi_core_r7(intent);
        let order = multi.then(|| run.request_order(intent, &open.profile));
        premises.extend(
            run.named_representatives(intent, &open.profile)
                .into_iter()
                .map(Premise::Named),
        );
        if intent.condition != Condition::Alone {
            premises.push(Premise::Unnamed);
        }
        premises.push(Premise::Inconclusive);
        let mut branches = Vec::new();
        for premise in premises {
            let mut sim = self.simulation();
            if !sim.end(intent, premise) {
                continue;
            }
            if premise == Premise::Unnamed && !sim.unattributed() {
                continue;
            }
            let mut settled = sim.settle();
            let resume = if premise == Premise::Pass {
                sim.clone().resume(&settled, cycle)
            } else {
                None
            };
            let mut passes = u32::from(premise == Premise::Pass);
            let mut all_pass = None;
            if premise == Premise::AllPass {
                passes = 1;
                let result = sim.pass_remaining(open, remaining, &mut settled, &mut passes);
                if matches!(result, AllPass::Diverged { .. }) {
                    continue;
                }
                all_pass = Some(result);
            }
            let named = if let Premise::Named(core) = premise {
                Some(core)
            } else {
                None
            };
            let failed = named.is_some() || premise == Premise::Unnamed;
            let without_telemetry = multi && premise != Premise::Inconclusive;
            let measured_by_offset = order
                .as_ref()
                .is_some_and(|order| !order.proxy().is_empty());
            let offset_order = without_telemetry
                && ((failed && measured_by_offset)
                    || settled.decisions.iter().any(|decision| {
                        matches!(decision, DecisionPayload::CheckingChain(chain) if chain.source_seqs.is_empty())
                    }));
            let top_requester = named
                .zip(order.as_ref())
                .is_some_and(|(core, order)| order.top().contains(core));
            branches.push(Branch {
                premise,
                passes,
                all_pass,
                at_zero: named.is_some_and(|core| Run::off_zero(&open.profile, core)),
                top_requester,
                without_telemetry,
                offset_order,
                decisions: settled.decisions,
                next: settled.next,
                next_step: settled.next_step,
                resume,
                uncertainty: settled.uncertainty,
            });
        }
        Forecast {
            branches,
            between: None,
        }
    }

    fn between(&self, cycle: Option<NonZeroU32>) -> Between {
        let mut sim = self.simulation();
        let settled = sim.settle();
        let resume = sim.resume(&settled, cycle);
        Between {
            decisions: settled.decisions,
            next: settled.next,
            next_step: settled.next_step,
            resume,
            uncertainty: settled.uncertainty,
        }
    }
}

impl Sim {
    fn unattributed(&self) -> bool {
        matches!(
            self.run().and_then(Run::attribution),
            Some(decision) if matches!(&decision.payload, DecisionPayload::Failure(failure) if failure.attribution == Attribution::Unattributed)
        )
    }
}

impl Run {
    fn next_unphased(&self) -> bool {
        self.topo
            .order()
            .iter()
            .any(|&core| matches!(self.core(core).phase, Phase::Unstarted))
    }
}
