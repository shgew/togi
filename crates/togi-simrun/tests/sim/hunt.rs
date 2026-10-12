use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use togi_config::Config;
use togi_journal::{
    Activity, CycleEvent, Decision, Event, Failure, HuntOutcome, HuntStage, Outcome, Payload,
    TrialIntent,
};
use togi_machine::{Condition, CoreId, CoreSet, PerCore, Regime, Signal, TargetOffset};
use togi_session::Stop;
use togi_sim::definition::JointDef;
use togi_sim::model::Script;
use togi_sim::{MachineDefinition, regime_index};
use togi_simrun::{Observe, Observer};
use togi_tuner::{Profile, State};

use crate::common::{Bench, finish, hunt_machine, nz};

const HUNT_BUDGET: usize = 4;
const CRASH_BUDGET: usize = 32;

pub(crate) fn core(id: i64) -> CoreId {
    CoreId::new(id).unwrap()
}

pub(crate) fn cores(ids: &[i64]) -> CoreSet {
    ids.iter().map(|id| core(*id)).collect()
}

pub(crate) fn config(candidate_solo_limits: &[(i64, i64)], cycle: Option<&[Regime]>) -> Config {
    let mut document = serde_json::Map::new();
    if !candidate_solo_limits.is_empty() {
        let limits: BTreeMap<String, i64> = candidate_solo_limits
            .iter()
            .map(|(id, limit)| (id.to_string(), *limit))
            .collect();
        document.insert("candidate_solo_limits".into(), serde_json::json!(limits));
    }
    if let Some(cycle) = cycle {
        document.insert("checking".into(), serde_json::json!({ "cycle": cycle }));
    }
    Config::parse(&serde_json::Value::Object(document).to_string()).unwrap()
}

pub(crate) fn all_cores(count: i64, limit: i64) -> Vec<(i64, i64)> {
    (0..count).map(|id| (id, limit)).collect()
}

fn checking_r1_first() -> Vec<Regime> {
    let mut cycle = Config::default().checking_cycle().as_slice().to_vec();
    let first = cycle
        .iter()
        .position(|regime| *regime == Regime::R1)
        .unwrap();
    cycle.swap(0, first);
    cycle
}

pub(crate) fn sharp_limits(definition: &mut MachineDefinition) {
    for limits in &mut definition.core {
        limits.alone = [-50; 5];
        limits.together = [-50; 7];
    }
}

fn joint(members: &[(i64, i64)]) -> JointDef {
    JointDef {
        members: members.iter().copied().collect(),
        regimes: vec![Regime::R6],
        rate: Some(10.0),
        ..JointDef::default()
    }
}

pub(crate) fn run(
    bench: &Bench,
    config: Config,
    observer: Option<Observer>,
) -> (Option<Stop>, Vec<Event>) {
    let mut input = bench.input();
    input.config = config;
    input.observer = observer;
    let finished = finish(input);
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    (finished.stop, bench.events())
}

fn phase_two_confirmed(event: &Event) -> bool {
    matches!(
        &event.payload,
        Payload::CheckingCycle(cycle)
            if cycle.event == CycleEvent::End
                && cycle.passed
                && cycle.full
                && cycle.reason.as_deref().is_some_and(|reason| reason.contains("phase 2 ends"))
    )
}

fn run_after_confirmation(
    definition: &MachineDefinition,
    joints: Vec<JointDef>,
    config: &Config,
) -> (Option<Stop>, Vec<Event>) {
    let first = Bench::new(definition, 1);
    let (_, confirmed) = run(
        &first,
        config.clone(),
        Some(Box::new(|event| {
            if phase_two_confirmed(event) {
                Observe::Stop
            } else {
                Observe::Continue
            }
        })),
    );
    assert!(confirmed.iter().any(phase_two_confirmed));
    let mut with_joints = definition.clone();
    with_joints.joint = joints;
    let second = first.resumed(&with_joints, 1);
    let mut input = second.input();
    input.config = config.clone();
    input.policy.cycles = Some(nz(2));
    let finished = finish(input);
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    (finished.stop, second.events())
}

fn profile_values(profile: &PerCore<TargetOffset>) -> Vec<i64> {
    profile.iter().map(|(_, offset)| offset.get()).collect()
}

pub(crate) fn adversarial_evidence(
    events: &[Event],
    hunt_budget: usize,
    crash_budget: usize,
) -> Vec<i64> {
    let mut state = State::new();
    let (mut hunts, mut crashes) = (0, 0);
    let mut last = None;
    for event in events {
        let profile = match &event.payload {
            Payload::TrialIntent(intent) => Some(intent.profile),
            Payload::ProfileChange(change) => {
                last = Some(change.to);
                Some(change.to)
            }
            Payload::HuntStart(_) => {
                hunts += 1;
                None
            }
            Payload::CrashDetected(_) => {
                crashes += 1;
                None
            }
            _ => None,
        };
        if let (Some(profile), Some(topology), Some(constraints)) =
            (profile, state.topology(), state.constraints())
        {
            let reached = constraints
                .reaches(&Profile::new(topology, profile).unwrap())
                .unwrap();
            assert!(
                reached.is_none(),
                "{} #{:?} reaches {reached:?}: {:?}",
                event.kind(),
                event.seq,
                profile_values(&profile)
            );
        }
        state.fold(event).unwrap();
    }
    assert!(
        hunts <= hunt_budget && crashes <= crash_budget,
        "session used {hunts} hunts and {crashes} crashes, budget {hunt_budget} and {crash_budget}"
    );
    let Some(last) = last else {
        panic!("the session never changed the profile");
    };
    profile_values(&last)
}

fn idle_budgets(definition: &MachineDefinition, core: usize) -> (usize, usize) {
    let limits = &definition.core[core];
    let hunts = usize::try_from(limits.idle.unwrap() - limits.together[0]).unwrap();
    (hunts, 2 * definition.core.len() + 3 * hunts)
}

fn idle_failures(events: &[Event], core: CoreId, idle: i64) -> Vec<&Failure> {
    let mut intents = BTreeMap::new();
    let mut failures = Vec::new();
    for event in events {
        match &event.payload {
            Payload::TrialIntent(intent) => {
                intents.insert(intent.trial.clone(), intent);
            }
            Payload::Failure(failure) => {
                let Some(intent) = failure.trial.as_ref().and_then(|trial| intents.get(trial))
                else {
                    continue;
                };
                let offset = intent.profile.get(core).map_or(0, TargetOffset::get);
                if intent.regime == Regime::R1
                    && intent.condition == Condition::Together
                    && offset < idle
                    && !intent.cores.contains(core)
                    && intent.core != Some(core)
                {
                    failures.push(failure);
                }
            }
            _ => {}
        }
    }
    failures
}

fn script(signal: Option<Signal>, core: CoreId) -> Script {
    Script {
        signal,
        at_s: 1.0,
        core,
        reset: None,
        then_crash: false,
    }
}

fn condition_limit_script(definition: &MachineDefinition, intent: &TrialIntent) -> Script {
    let offset = |core: CoreId| intent.profile.get(core).map_or(0, TargetOffset::get);
    let loaded: Vec<CoreId> = match intent.core {
        Some(core) => vec![core],
        None => intent.cores.iter().collect(),
    };
    let regime = regime_index(intent.regime);
    let alone = intent.condition == Condition::Alone;
    for core in &loaded {
        let limits = &definition.core[core.index()];
        let limit = if alone {
            limits.alone[regime]
        } else {
            limits.together[regime]
        };
        if offset(*core) < limit {
            return script(Some(Signal::ComputationError), *core);
        }
    }
    if alone {
        return script(None, core(0));
    }
    for (index, limits) in definition.core.iter().enumerate() {
        let id = core(i64::try_from(index).unwrap());
        if limits.idle.is_some_and(|idle| offset(id) < idle) && !loaded.contains(&id) {
            return script(Some(Signal::ComputationError), id);
        }
    }
    for joint in &definition.joint {
        let reached = joint.regimes.contains(&intent.regime)
            && joint
                .members
                .iter()
                .all(|(member, threshold)| offset(core(*member)) <= *threshold);
        if reached {
            return script(Some(Signal::Crash), core(0));
        }
    }
    script(None, core(0))
}

fn scripting_observer(bench: &Bench, definition: &MachineDefinition) -> Observer {
    let sim = bench.sim.clone();
    let definition = definition.clone();
    Box::new(move |event| {
        if let Payload::TrialIntent(intent) = &event.payload {
            sim.script_trial(
                intent.trial.as_str(),
                condition_limit_script(&definition, intent),
            )
            .unwrap();
        }
        Observe::Continue
    })
}

#[test]
fn a_hunt_without_a_clean_cycle_parks_every_offset_at_zero() {
    let mut definition = hunt_machine(16);
    definition.core[11].together[5] = -5;
    let bench = Bench::new(&definition, 1);
    let (stop, events) = run(&bench, Config::default(), None);
    assert_eq!(stop, Some(Stop::Cycles));
    let start = find!(events, HuntStart, |p| p.parked_seq.is_none())
        .expect("no hunt with all-zero parked offsets");
    assert!(
        start
            .parked
            .iter()
            .all(|(_, offset)| offset == TargetOffset::ZERO),
        "{:?}",
        profile_values(&start.parked)
    );
    let end = find!(events, HuntEnd, |p| p.hunt == start.hunt
        && p.result == HuntOutcome::Direct)
    .expect("the hunt never ended directly");
    assert_eq!(end.cores, cores(&[11]));
    assert!(
        find!(events, TunerDecision, |p| p.decision == Decision::Backoff
            && p.core == core(11))
        .is_some(),
        "core 11 not backed off"
    );
    assert!(
        find!(events, CheckingCycle, |p| p.event == CycleEvent::End
            && p.passed
            && p.full)
        .is_some(),
        "no passed full cycle"
    );
}

fn culprit_machine() -> (MachineDefinition, Config) {
    let mut definition = hunt_machine(4);
    sharp_limits(&mut definition);
    (definition, config(&all_cores(4, -10), None))
}

#[test]
fn a_hunt_after_a_clean_cycle_parks_the_offsets_of_that_cycle_and_finds_the_culprit() {
    let (definition, config) = culprit_machine();
    let (stop, events) = run_after_confirmation(&definition, vec![joint(&[(1, -10)])], &config);
    assert_eq!(stop, Some(Stop::Cycles));
    let start = find!(events, HuntStart, |p| p.parked_seq.is_some())
        .expect("no hunt with parked offsets from a clean cycle");
    let end = find!(events, HuntEnd, |p| p.hunt == start.hunt
        && p.result == HuntOutcome::Culprit)
    .expect("the hunt did not end with a culprit");
    assert_eq!(end.cores, cores(&[1]));
    assert!(
        find!(events, TunerDecision, |p| p.core == core(1)
            && p.decision == Decision::Backoff
            && p.failure_point
                .is_some_and(|point| p.to_offset.get() == point.get() + 1))
        .is_some(),
        "no one-count-shallower backoff of core 1"
    );
}

#[test]
fn a_failure_at_a_held_cores_parked_offset_raises_the_parked_offsets_of_the_next_hunt() {
    let (mut definition, config) = culprit_machine();
    let joints = vec![joint(&[(1, -10)])];
    let (_, probe) = run_after_confirmation(&definition, joints.clone(), &config);
    let first = find!(probe, HuntStart, |p| p.parked_seq.is_some())
        .expect("no parked offsets from a clean cycle");
    let group = find!(probe, HuntGroup, |p| p.hunt == first.hunt
        && p.cores.len() < first.candidates.len())
    .expect("no partial group");
    let trial = probe
        .iter()
        .find_map(|event| match &event.payload {
            Payload::TrialIntent(intent)
                if intent.hunt == Some(first.hunt) && intent.group == Some(group.group) =>
            {
                Some(intent.trial.clone())
            }
            _ => None,
        })
        .expect("no parked trial of the partial group");
    let held = first
        .candidates
        .iter()
        .find(|candidate| {
            !group.cores.contains(*candidate)
                && first
                    .parked
                    .get(*candidate)
                    .is_some_and(|offset| offset != TargetOffset::ZERO)
        })
        .expect("no held core at a nonzero parked offset");
    definition.script = vec![togi_sim::definition::ScriptDef {
        trial: trial.to_string(),
        signal: Some(Signal::ComputationError),
        at_s: 1.0,
        core: i64::from(held.get()),
        ..Default::default()
    }];
    let (_, events) = run_after_confirmation(&definition, joints, &config);
    let parked_at_held = first.parked.get(held).unwrap();
    let end = find!(events, HuntEnd, |p| p.hunt == first.hunt
        && p.result == HuntOutcome::Direct
        && p.cores == cores(&[i64::from(held.get())]))
    .expect("the hunt did not directly attribute the held core");
    assert_eq!(end.cores.len(), 1);
    assert!(
        find!(events, TunerDecision, |p| p.core == held
            && p.decision == Decision::Backoff
            && p.failure_point == Some(parked_at_held))
        .is_some(),
        "no failure point at the held core's parked offset"
    );
    let next = find!(events, HuntStart, |p| p.hunt > first.hunt).expect("no later hunt");
    assert_eq!(next.parked_seq, first.parked_seq);
    let raised = next.parked.get(held).unwrap();
    assert!(
        raised > parked_at_held,
        "{raised:?} is not past {parked_at_held:?}"
    );
    assert_eq!(Some(raised), next.failing.get(held));
    assert!(
        !next.candidates.contains(held),
        "the next candidates include the held core, no deeper than its raised parked offset"
    );
}

#[test]
fn a_joint_crash_makes_a_combination_instead_of_single_core_failure_points() {
    let mut definition = hunt_machine(16);
    definition.ranking = Some(vec![0, 1, 2, 11, 4, 5, 6, 7, 8, 9, 10, 3, 12, 13, 14, 15]);
    sharp_limits(&mut definition);
    definition.joint = vec![joint(&[(3, -10), (11, -10)])];
    let bench = Bench::new(&definition, 1);
    let (stop, events) = run(&bench, Config::default(), None);
    assert_eq!(stop, Some(Stop::Cycles));
    let combination = find!(events, Combination, |p| !p.fallback && p.members.len() == 2)
        .expect("no combination of two cores");
    let members: Vec<_> = combination
        .members
        .iter()
        .map(|member| (member.core.get(), member.offset.get()))
        .collect();
    assert_eq!(members, [(3, -10), (11, -10)]);
    let (mut combinations, mut backoffs) = (0, 0);
    for event in &events {
        match &event.payload {
            Payload::Combination(p) if p.combination == combination.combination => {
                combinations += 1;
            }
            Payload::TunerDecision(p) => {
                assert!(
                    p.failure_point.is_none(),
                    "joint crash produced a single-core failure point: {p:?}"
                );
                if p.decision == Decision::Backoff && p.activity == Activity::Hunt {
                    backoffs += 1;
                }
            }
            Payload::TrialIntent(p) if combinations > 0 => {
                let at = |id| p.profile.get(core(id)).map_or(0, TargetOffset::get);
                assert!(
                    !(at(3) <= -10 && at(11) <= -10),
                    "trial {} reaches the combination",
                    p.trial
                );
            }
            _ => {}
        }
    }
    assert_eq!((combinations, backoffs), (1, 1));
    let mut want = vec![-50; 16];
    want[3] = -9;
    assert_eq!(adversarial_evidence(&events, 1, CRASH_BUDGET), want);
}

#[test]
fn a_joint_crash_with_a_misleading_core_local_mce_blames_that_core_only() {
    let mut definition = hunt_machine(16);
    definition.joint = vec![JointDef {
        crash_mce_core: Some(3),
        ..joint(&[(3, -10), (11, -10)])
    }];
    let bench = Bench::new(&definition, 1);
    let (stop, events) = run(&bench, Config::default(), None);
    assert_eq!(stop, Some(Stop::Cycles));
    assert!(
        find!(events, TunerDecision, |p| p.core == core(3)
            && p.failure_point.is_some_and(|point| point.get() == -10))
        .is_some(),
        "the misleading joint MCE did not attribute core 3 at -10"
    );
    assert!(
        find!(events, Combination).is_none(),
        "misleading core-local evidence produced a combination"
    );
    let mut want = vec![-10; 16];
    want[3] = -9;
    let two_alone_search_failures_per_core_and_the_joint_crash = 2 * 16 + 1;
    assert_eq!(
        adversarial_evidence(
            &events,
            1,
            two_alone_search_failures_per_core_and_the_joint_crash
        ),
        want
    );
}

fn delayed_hunt_escalates(after_s: f64) {
    let definition = hunt_machine(4);
    let mut delayed = joint(&[(1, -10)]);
    delayed.after_s = after_s;
    let (stop, events) = run_after_confirmation(&definition, vec![delayed], &Config::default());
    assert_eq!(stop, Some(Stop::Cycles));
    assert_eq!(
        adversarial_evidence(&events, HUNT_BUDGET, CRASH_BUDGET),
        [-10, -9, -10, -10]
    );
    let mut full_groups: BTreeSet<(NonZeroU32, NonZeroU32)> = BTreeSet::new();
    let mut full_trials = BTreeSet::new();
    let (mut full_passes, mut escalated, mut resolved) = (0, false, false);
    for event in &events {
        match &event.payload {
            Payload::HuntGroup(group) => {
                if group.stage == HuntStage::Full && !group.escalated {
                    full_groups.insert((group.hunt, group.group));
                }
                escalated |= group.escalated && group.duration_s == 900;
            }
            Payload::TrialIntent(intent) => {
                if let (Some(hunt), Some(group)) = (intent.hunt, intent.group)
                    && full_groups.contains(&(hunt, group))
                {
                    full_trials.insert(intent.trial.clone());
                }
            }
            Payload::TrialEnd(end) => {
                if full_trials.contains(&end.trial) && end.outcome == Outcome::Pass {
                    full_passes += 1;
                }
            }
            Payload::HuntEnd(end) => {
                resolved |= matches!(end.result, HuntOutcome::Culprit | HuntOutcome::Direct)
                    && end.cores == cores(&[1]);
            }
            _ => {}
        }
    }
    assert_eq!(full_passes, 5);
    assert!(escalated, "no escalated 900 s group");
    assert!(resolved, "core 1 was not resolved");
}

#[test]
fn a_hunt_of_a_joint_that_fails_after_six_idle_minutes_escalates() {
    delayed_hunt_escalates(360.0);
}

#[test]
fn a_hunt_of_a_joint_that_fails_at_the_short_duration_boundary_escalates() {
    delayed_hunt_escalates(120.0);
}

fn scripted_session(definition: &MachineDefinition, config: Config) -> (Option<Stop>, Vec<Event>) {
    let bench = Bench::new(definition, 1);
    let observer = scripting_observer(&bench, definition);
    run(&bench, config, Some(observer))
}

#[test]
fn a_scripted_sharp_joint_limit_is_found_in_ranking_order() {
    let mut definition = hunt_machine(4);
    definition.ranking = Some(vec![2, 1, 0, 3]);
    definition.joint = vec![joint(&[(0, -6), (2, -6)])];
    let (stop, events) = scripted_session(&definition, Config::default());
    assert_eq!(stop, Some(Stop::Cycles));
    assert_eq!(
        adversarial_evidence(&events, HUNT_BUDGET, CRASH_BUDGET),
        [-5, -10, -10, -10]
    );
}

#[test]
fn a_scripted_idle_only_limit_outside_the_loaded_cores_is_found_by_r1() {
    let mut definition = hunt_machine(4);
    definition.ranking = Some(vec![2, 1, 0, 3]);
    definition.core[3].idle = Some(-5);
    let config = config(&[], Some(&checking_r1_first()));
    let (stop, events) = scripted_session(&definition, config);
    assert_eq!(stop, Some(Stop::Cycles));
    let (hunt_budget, crash_budget) = idle_budgets(&definition, 3);
    assert_eq!(
        adversarial_evidence(&events, hunt_budget, crash_budget),
        [-10, -10, -10, -5]
    );
    assert!(
        idle_failures(&events, core(3), -5)
            .iter()
            .any(|failure| failure.core == Some(core(3))
                && failure.signal == Signal::ComputationError),
        "no named R1 idle-limit failure outside the loaded cores"
    );
}

fn idle_only_hazard(seed: u64) {
    let mut definition = hunt_machine(4);
    definition.core[3].idle = Some(-5);
    let bench = Bench::new(&definition, seed);
    let (stop, events) = run(&bench, config(&[], Some(&checking_r1_first())), None);
    assert_eq!(stop, Some(Stop::Cycles));
    let (hunt_budget, crash_budget) = idle_budgets(&definition, 3);
    assert_eq!(
        adversarial_evidence(&events, hunt_budget, crash_budget),
        [-10, -10, -10, -5]
    );
    assert!(
        !idle_failures(&events, core(3), -5).is_empty(),
        "the idle-only hazard never failed outside the loaded cores"
    );
}

#[test]
fn an_idle_only_hazard_reaches_a_clean_cycle_at_seed_1() {
    idle_only_hazard(1);
}

#[test]
fn an_idle_only_hazard_reaches_a_clean_cycle_at_seed_2() {
    idle_only_hazard(2);
}
