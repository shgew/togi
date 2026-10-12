use std::collections::BTreeMap;

use togi_journal::{Attribution, Event, Outcome, Payload, Seq, TrialEnd, TrialId, TrialIntent};
use togi_machine::{Condition, CoreId, Regime, Signal};
use togi_sim::MachineDefinition;
use togi_sim::definition::{CoreDef, JointDef, ModelDef, Weights};
use togi_simrun::{SimOutcome, simulate};
use togi_tuner::{AllPass, Branch, DecisionPayload, Premise, State, TrialPlan, Uncertainty};

use crate::common::Bench;

const CRASH_JOINT_SEED: u64 = 7;
const DEFAULT_CORES: i64 = 16;
const MIN_CHECKED: usize = 50;

fn machine(seed: u64, cores: i64) -> MachineDefinition {
    if cores == DEFAULT_CORES {
        return MachineDefinition::default();
    }
    let limits = (0..cores)
        .map(|id| CoreDef {
            id,
            alone: [-10; 5],
            together: [-10; 7],
            flat: 0.0,
            idle: None,
            workload: BTreeMap::new(),
        })
        .collect();
    let mut model = ModelDef {
        past_limit_rate: 1.0,
        signals: Weights::from([(Signal::ComputationError, 1.0)]),
        ..ModelDef::default()
    };
    let mut joint = Vec::new();
    if seed == CRASH_JOINT_SEED {
        model.crash_mce = 0.0;
        joint.push(JointDef {
            members: BTreeMap::from([(0, -10), (1, -10)]),
            regimes: vec![Regime::R7],
            rate: Some(1.0),
            signal: Some(Signal::Crash),
            ..JointDef::default()
        });
    }
    MachineDefinition {
        cores,
        core: limits,
        model,
        joint,
        ..MachineDefinition::default()
    }
}

fn simulated(seed: u64, cores: i64) -> Vec<Event> {
    let bench = Bench::new(&machine(seed, cores), seed);
    match simulate(bench.input()).unwrap() {
        SimOutcome::Stopped(_) => bench.events(),
        SimOutcome::Capped { boots } => panic!("capped after {boots} boots"),
    }
}

fn decision(payload: &Payload) -> Option<DecisionPayload> {
    Some(match payload {
        Payload::Failure(p) => p.clone().into(),
        Payload::TunerDecision(p) => p.clone().into(),
        Payload::CorePhase(p) => p.clone().into(),
        Payload::CheckingCycle(p) => p.clone().into(),
        Payload::CheckingStep(p) => p.clone().into(),
        Payload::CheckingChain(p) => p.clone().into(),
        Payload::ProfileChange(p) => p.clone().into(),
        Payload::HuntStart(p) => p.clone().into(),
        Payload::HuntGroup(p) => p.clone().into(),
        Payload::HuntEnd(p) => p.clone().into(),
        Payload::HuntSkipped(p) => p.clone().into(),
        Payload::Combination(p) => p.clone().into(),
        Payload::DeepeningRound(p) => p.clone().into(),
        Payload::TunerWarning(p) => p.clone().into(),
        Payload::DeadEnd(p) => p.clone().into(),
        _ => return None,
    })
}

fn comparable(decision: DecisionPayload) -> DecisionPayload {
    match decision {
        DecisionPayload::Failure(mut p) => {
            p.signal = Signal::Crash;
            p.trial = None;
            p.reason = None;
            p.known_failure = None;
            p.into()
        }
        DecisionPayload::TunerDecision(mut p) => {
            p.reason.clear();
            p.into()
        }
        DecisionPayload::CorePhase(mut p) => {
            p.reason.clear();
            p.into()
        }
        DecisionPayload::CheckingCycle(mut p) => {
            p.reason = None;
            p.into()
        }
        DecisionPayload::CheckingChain(mut p) => {
            p.source_seqs.clear();
            p.into()
        }
        DecisionPayload::HuntStart(mut p) => {
            p.failure = Seq::FIRST;
            p.parked_seq = None;
            p.trial = None;
            p.reason = None;
            p.into()
        }
        DecisionPayload::HuntGroup(mut p) => {
            p.reason = None;
            p.into()
        }
        DecisionPayload::HuntEnd(mut p) => {
            p.reason.clear();
            p.into()
        }
        DecisionPayload::HuntSkipped(mut p) => {
            p.failure = Seq::FIRST;
            p.reason.clear();
            p.into()
        }
        DecisionPayload::Combination(mut p) => {
            p.reason.clear();
            p.into()
        }
        DecisionPayload::DeepeningRound(mut p) => {
            p.base_seq = None;
            p.reason = None;
            p.into()
        }
        DecisionPayload::TunerWarning(mut p) => {
            p.trial = None;
            p.passes.clear();
            p.detail.clear();
            p.into()
        }
        DecisionPayload::DeadEnd(mut p) => {
            p.detail.clear();
            p.into()
        }
        other @ (DecisionPayload::CheckingStep(_) | DecisionPayload::ProfileChange(_)) => other,
    }
}

fn journal_decisions(decisions: &[DecisionPayload]) -> Vec<DecisionPayload> {
    decisions.iter().cloned().map(comparable).collect()
}

fn trial_end<'a>(
    events: &'a [Event],
    after: usize,
    trial: &TrialId,
) -> Option<(usize, &'a TrialEnd)> {
    events
        .iter()
        .enumerate()
        .skip(after + 1)
        .find_map(|(at, event)| match &event.payload {
            Payload::TrialEnd(end) if end.trial == *trial => Some((at, end)),
            _ => None,
        })
}

struct Followed {
    decisions: Vec<DecisionPayload>,
    next: TrialIntent,
}

fn all_pass_journal(
    events: &[Event],
    end_at: usize,
    passes: u32,
    ranking: &[CoreId],
) -> Option<Followed> {
    let mut ran = 1;
    let mut decisions = Vec::new();
    let mut at = end_at + 1;
    while at < events.len() {
        match &events[at].payload {
            Payload::TrialIntent(intent) => {
                if ran == passes {
                    return Some(Followed {
                        decisions,
                        next: intent.clone(),
                    });
                }
                let (end_at, end) = trial_end(events, at, &intent.trial)?;
                if end.outcome != Outcome::Pass {
                    return None;
                }
                ran += 1;
                at = end_at + 1;
                continue;
            }
            Payload::HostRanking(read) if read.ranking != ranking => return None,
            Payload::Shutdown(_) => return None,
            payload => decisions.extend(decision(payload)),
        }
        at += 1;
    }
    None
}

fn normalized(mut intent: TrialIntent) -> TrialIntent {
    intent.kernel_cursor = None;
    intent.kernel_error = None;
    intent
}

fn assert_next(trial: &TrialId, next: &TrialIntent, forecast: Option<&TrialPlan>) {
    let plan =
        forecast.unwrap_or_else(|| panic!("trial {trial} forecast stopped before {}", next.trial));
    assert_eq!(
        normalized(next.clone()),
        plan.intent(next.trial.clone(), 0),
        "trial {trial} next intent (left journal, right forecast)"
    );
}

fn assert_decisions(
    trial: &TrialId,
    what: &str,
    journal: &[DecisionPayload],
    forecast: &[DecisionPayload],
) {
    assert_eq!(
        journal_decisions(journal),
        journal_decisions(forecast),
        "trial {trial} {what} decisions (left journal, right forecast)"
    );
}

#[derive(Default)]
struct Tally {
    checked: usize,
    all_pass: usize,
    unavailable: usize,
}

fn telemetry_unavailable(intent: &TrialIntent, end: &TrialEnd) -> bool {
    intent.regime == Regime::R7
        && intent.core.is_none()
        && intent.cores.len() > 1
        && end.outcome != Outcome::Inconclusive
        && (!end.voltage_requests_v.is_empty()
            || !end.top_requesters.is_empty()
            || !end.ccd_mhz.is_empty())
}

enum Wanted {
    Pass,
    Inconclusive,
    Unnamed,
    Named(Option<CoreId>),
}

fn wanted(events: &[Event], intent: &TrialIntent, end_at: usize, end: &TrialEnd) -> Wanted {
    match end.outcome {
        Outcome::Pass => Wanted::Pass,
        Outcome::Inconclusive => Wanted::Inconclusive,
        Outcome::Failure => {
            let mut named = end.core.is_some()
                || (intent.core.is_some() && intent.condition == Condition::Alone);
            let mut core = end.core;
            for event in &events[end_at + 1..] {
                match &event.payload {
                    Payload::TrialIntent(_) => break,
                    Payload::Failure(failure) if failure.trial.as_ref() == Some(&intent.trial) => {
                        core = failure.core;
                        named |= failure.attribution == Attribution::Attributed;
                        break;
                    }
                    _ => {}
                }
            }
            if named {
                Wanted::Named(core)
            } else {
                Wanted::Unnamed
            }
        }
    }
}

fn selects(wanted: &Wanted, premise: Premise) -> bool {
    match (wanted, premise) {
        (Wanted::Pass, Premise::Pass)
        | (Wanted::Inconclusive, Premise::Inconclusive)
        | (Wanted::Unnamed, Premise::Unnamed) => true,
        (Wanted::Named(Some(core)), Premise::Named(found)) => *core == found,
        _ => false,
    }
}

fn check(seed: u64, cores: i64, every: usize) {
    let events = simulated(seed, cores);
    let mut tally = Tally::default();
    let mut state = State::new();
    let mut folded = 0;
    let mut cycle_state = State::new();
    let mut cycle_folded = 0;
    let mut ranking: Vec<CoreId> = Vec::new();
    let mut intents = 0;
    for (i, event) in events.iter().enumerate() {
        if let Payload::HostRanking(read) = &event.payload {
            ranking.clone_from(&read.ranking);
        }
        let Payload::TrialIntent(intent) = &event.payload else {
            continue;
        };
        intents += 1;
        if intents % every != 0 {
            continue;
        }
        let (end_at, end) = trial_end(&events, i, &intent.trial)
            .unwrap_or_else(|| panic!("trial {} has no end", intent.trial));
        if telemetry_unavailable(intent, end) {
            tally.unavailable += 1;
            continue;
        }
        for folding in &events[folded..=i] {
            state.fold(folding).unwrap();
        }
        folded = i + 1;
        let forecast = state.forecast();
        let wanted = wanted(&events, intent, end_at, end);
        let mut branch: Option<&Branch> = None;
        let mut any_named = false;
        let mut all: Option<&Branch> = None;
        for candidate in &forecast.branches {
            assert_ne!(
                candidate.uncertainty,
                Some(Uncertainty::NeedsHistory),
                "complete simulated history recurred at trial {}, premise {:?}",
                intent.trial,
                candidate.premise
            );
            if candidate.premise == Premise::AllPass {
                all = Some(candidate);
            }
            any_named |= matches!(candidate.premise, Premise::Named(_));
            if selects(&wanted, candidate.premise) {
                branch = Some(candidate);
            }
        }
        let Some(branch) = branch else {
            assert!(
                matches!(wanted, Wanted::Named(_)) && any_named,
                "trial {} lacks the premise its end shows: intent {intent:?} end {end:?}",
                intent.trial
            );
            tally.unavailable += 1;
            continue;
        };
        if let Some(all) = all
            && end.outcome == Outcome::Pass
            && all.all_pass == Some(AllPass::Complete)
            && let Some(followed) = all_pass_journal(&events, end_at, all.passes, &ranking)
        {
            assert_decisions(
                &intent.trial,
                "all-pass",
                &followed.decisions,
                &all.decisions,
            );
            assert_next(&intent.trial, &followed.next, all.next.as_ref());
            tally.all_pass += 1;
        }
        let mut decisions = Vec::new();
        let mut next: Option<(usize, &TrialIntent)> = None;
        let mut ranking_changed = false;
        for (at, later) in events.iter().enumerate().skip(end_at + 1) {
            match &later.payload {
                Payload::TrialIntent(found) => {
                    next = Some((at, found));
                    break;
                }
                Payload::HostRanking(read) => ranking_changed = read.ranking != ranking,
                Payload::Shutdown(_) => break,
                _ => {}
            }
            decisions.extend(decision(&later.payload));
        }
        if matches!(
            branch.uncertainty,
            Some(Uncertainty::NeedsRanking | Uncertainty::RecurringPlan)
        ) || ranking_changed
        {
            tally.unavailable += 1;
            continue;
        }
        let Some((next_at, next)) = next else {
            if end.outcome == Outcome::Pass {
                assert!(
                    branch.decisions.len() >= decisions.len(),
                    "trial {} lost decisions before stop",
                    intent.trial
                );
                assert_decisions(
                    &intent.trial,
                    "pre-stop",
                    &decisions,
                    &branch.decisions[..decisions.len()],
                );
            } else {
                assert_decisions(&intent.trial, "final", &decisions, &branch.decisions);
                assert!(
                    branch.next.is_none(),
                    "trial {} forecast ran after stop",
                    intent.trial
                );
            }
            continue;
        };
        assert_decisions(&intent.trial, "next", &decisions, &branch.decisions);
        assert_next(&intent.trial, next, branch.next.as_ref());
        if next.cycle.is_some() && !next.rerun && next.hunt.is_none() {
            assert!(
                next_at + 1 >= cycle_folded,
                "trial {} next intent at {next_at} precedes folded prefix {cycle_folded}",
                intent.trial
            );
            for folding in &events[cycle_folded..=next_at] {
                cycle_state.fold(folding).unwrap();
            }
            cycle_folded = next_at + 1;
            let want = cycle_state.project().cycle_plan.current + 1;
            assert_eq!(
                branch.next_step.map(|step| step.get().get() as usize),
                Some(want),
                "trial {} next step when it runs",
                intent.trial
            );
        }
        tally.checked += 1;
    }
    assert!(
        tally.checked >= MIN_CHECKED && tally.all_pass > 0,
        "seed {seed}, {cores} cores: only {} predictions and {} all-pass predictions checked ({} unavailable inputs)",
        tally.checked,
        tally.all_pass,
        tally.unavailable
    );
}

#[test]
fn seed_1_two_cores() {
    check(1, 2, 1);
}

#[test]
fn seed_7_two_cores_crash_joint() {
    check(7, 2, 1);
}

#[test]
fn seed_1_default_cores_every_eighth() {
    check(1, DEFAULT_CORES, 8);
}
