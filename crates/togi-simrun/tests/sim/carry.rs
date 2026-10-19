use std::collections::{BTreeMap, BTreeSet, HashMap};

use togi_config::Config;
use togi_journal::fs::{Fault, Op};
use togi_journal::{
    Activity, Attribution, Build, CarriedCore, CorePhase, CoreTransition, CycleEvent,
    DeadEndCondition, Event, EventKind, HuntOutcome, Outcome, Payload, SessionCarried, SessionId,
    TrialClass, TrialId, TrialIntent,
};
use togi_machine::{
    BiosContext, Condition, ContextField, CoreId, PerCore, Regime, Signal, TargetOffset, WorkloadId,
};
use togi_session::{PrepareError, Stop};
use togi_sim::MachineDefinition;
use togi_sim::definition::JointDef;
use togi_simrun::{Observer, SimError, SimInput, simulate};
use togi_tuner::PhaseNo;

use crate::common::{
    Bench, STATE, build, finish, hunt_machine, machine_file, phase_two_concludes, stop_when,
    until_kind,
};

const SEED: u64 = 1000;

fn previous_build() -> Build {
    build()
}

fn next_build() -> Build {
    Build {
        ruleset: previous_build().ruleset + 1,
        ..previous_build()
    }
}

fn next_input(bench: &Bench) -> SimInput {
    let mut input = bench.input();
    input.build = next_build();
    input
}

fn core(id: i64) -> CoreId {
    CoreId::new(id).unwrap()
}

fn offset(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}

fn uniform(value: i64, cores: i64) -> PerCore<TargetOffset> {
    (0..cores).map(|id| (core(id), offset(value))).collect()
}

fn shared_voltage() -> MachineDefinition {
    machine_file("shared-voltage")
}

fn clean_cycle() -> Bench {
    let bench = Bench::new(&shared_voltage(), SEED);
    finish(bench.input());
    bench
}

fn changed_bios() -> BiosContext {
    BiosContext {
        bios_version: "changed".into(),
        board: "board".into(),
        cpu_model: "cpu".into(),
        microcode: "0x1".into(),
        boost_limit_mhz: 5000,
    }
}

fn after_initial_phases(cores: usize) -> Observer {
    let mut seen = 0;
    stop_when(move |event| {
        if matches!(&event.payload, Payload::CorePhase(phase) if phase.from.is_none()) {
            seen += 1;
        }
        seen == cores
    })
}

fn after_alone_passes(cores: usize) -> Observer {
    let mut trials = BTreeMap::new();
    let mut passed = BTreeSet::new();
    stop_when(move |event| {
        let payload = &event.payload;
        if let Payload::TrialIntent(intent) = payload
            && intent.condition == Condition::Alone
        {
            if let Some(core) = intent.core {
                trials.insert(intent.trial.clone(), core);
            }
        } else if let Payload::TrialEnd(end) = payload
            && end.outcome == Outcome::Pass
        {
            if let Some(core) = trials.get(&end.trial) {
                passed.insert(*core);
            }
        }
        passed.len() == cores
    })
}

fn is_passed_full_cycle(event: &Event) -> bool {
    matches!(
        &event.payload,
        Payload::CheckingCycle(cycle)
            if cycle.event == CycleEvent::End && cycle.passed && cycle.full
    )
}

fn session_of(events: &[Event]) -> SessionId {
    let payload = &events
        .first()
        .unwrap_or_else(|| panic!("a journal"))
        .payload;
    let Payload::SessionStart(start) = payload else {
        panic!("first event is {payload:?}");
    };
    start.session
}

fn archived(bench: &Bench, session: SessionId) -> Option<Vec<u8>> {
    bench
        .fs
        .contents(format!("{STATE}/archive/{session}.jsonl"))
}

fn carried(events: &[Event]) -> SessionCarried {
    let all: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::SessionCarried(carried) = &event.payload {
                Some(carried.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(all.len(), 1, "session.carried events");
    all.into_iter().next().unwrap()
}

fn carried_core(events: &[Event], id: CoreId) -> CarriedCore {
    carried(events)
        .carried
        .into_iter()
        .find(|seed| seed.core == id)
        .unwrap_or_else(|| panic!("{id:?} carries nothing"))
}

fn first_phases(events: &[Event]) -> BTreeMap<CoreId, CoreTransition> {
    let mut phases = BTreeMap::new();
    for event in events {
        if let Payload::CorePhase(phase) = &event.payload
            && phase.from.is_none()
        {
            phases.entry(phase.core).or_insert_with(|| phase.clone());
        }
    }
    phases
}

fn carried_start(limit: i64, failure_point: Option<i64>) -> i64 {
    match failure_point {
        Some(point) if point < 0 && limit <= point => point + 1,
        _ => limit,
    }
}

fn config(text: &str) -> Config {
    Config::parse(text).unwrap()
}

fn candidates(limit: i64, cores: i64) -> serde_json::Value {
    (0..cores)
        .map(|core| (core.to_string(), serde_json::Value::from(limit)))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

fn candidate_config(limit: i64, cores: i64) -> Config {
    config(&serde_json::json!({ "candidate_solo_limits": candidates(limit, cores) }).to_string())
}

fn quick_matrix() -> Config {
    config(
        &serde_json::json!({
            "candidate_solo_limits": candidates(-10, 4),
            "evidence": {"miss": 0.5},
            "durations": {
                "search_trial_s": 1,
                "short_trial_s": 1,
                "checking_trial_s": 1,
                "checking_idle_s": 1,
                "checking_all_core_s": 4
            }
        })
        .to_string(),
    )
}

#[test]
fn a_ruleset_transition_seeds_the_next_session() {
    let source = clean_cycle();
    let id = session_of(&source.events());
    let next = source.resumed(&shared_voltage(), SEED);
    let finished = finish(next_input(&next));
    assert_eq!(finished.stop, Some(Stop::Cycles));
    assert!(archived(&next, id).is_some());
    let events = next.events();
    let carried = carried(&events);
    assert_eq!(carried.sources.len(), 1);
    assert_eq!(carried.sources[0].session, id);
    assert!(carried.failure_points);
    let phases = first_phases(&events);
    let mut failure_points = BTreeMap::new();
    let (mut limits, mut points) = (0, 0);
    for seed in &carried.carried {
        let phase = &phases[&seed.core];
        if let Some(point) = seed.failure_point {
            points += 1;
            failure_points.insert(seed.core, point.offset);
            assert_eq!(phase.failure_point, Some(point.offset), "{:?}", seed.core);
            assert!(phase.offset > point.offset, "{:?}: {phase:?}", seed.core);
        }
        if let Some(limit) = seed.candidate_solo_limit {
            limits += 1;
            let want = carried_start(
                limit.offset.get(),
                seed.failure_point.map(|point| point.offset.get()),
            );
            assert_eq!(phase.to, CorePhase::Search, "{:?}", seed.core);
            assert!(phase.check_solo_limit, "{:?}", seed.core);
            assert_eq!(phase.offset.get(), want, "{:?}", seed.core);
        }
    }
    assert!(
        limits > 0 && points > 0,
        "{limits} limits, {points} failure points"
    );
    let mut tested = 0;
    for event in &events {
        if let Payload::TrialIntent(intent) = &event.payload
            && intent.condition == Condition::Alone
            && let (Some(core), Some(tested_at)) = (intent.core, intent.offset)
            && let Some(&point) = failure_points.get(&core)
        {
            tested += 1;
            assert!(
                tested_at > point,
                "seq {}: {core:?} tested at {tested_at:?}",
                event.seq
            );
        }
    }
    assert!(tested > 0);
}

#[test]
fn a_previous_ruleset_session_transitions_to_a_seeded_session() {
    let source = clean_cycle();
    let source_events = source.events();
    let id = session_of(&source_events);
    let decisions: BTreeMap<CoreId, _> = togi_tuner::replay(&source_events)
        .unwrap()
        .project()
        .cores
        .into_iter()
        .map(|view| (view.core, (view.offset, view.pass)))
        .collect();
    let next = source.resumed(&shared_voltage(), SEED);
    let finished = finish(next_input(&next));
    assert_eq!(finished.stop, Some(Stop::Cycles));
    assert!(archived(&next, id).is_some());
    let events = next.events();
    let Payload::SessionStart(start) = &events[0].payload else {
        panic!("first event is {:?}", events[0].payload);
    };
    assert_eq!(start.build, next_build());
    assert_ne!(start.session, id);
    let carried = carried(&events);
    assert_eq!(carried.sources.len(), 1);
    assert_eq!(carried.sources[0].session, id);
    assert_eq!(carried.sources[0].ruleset, previous_build().ruleset);
    let mut carried_at = None;
    let (mut facts, mut live_trials) = (0, 0);
    for event in &events {
        let payload = &event.payload;
        if let Payload::SessionCarried(_) = payload {
            carried_at = Some(event.seq);
        } else if matches!(
            payload,
            Payload::TrialCarried(_) | Payload::FailureCarried(_)
        ) {
            facts += 1;
            assert_eq!(
                carried_at, None,
                "seq {} follows session.carried",
                event.seq
            );
        } else if let Payload::TrialEnd(_) = payload {
            live_trials += 1;
        } else if let Payload::CheckingCycle(cycle) = payload
            && cycle.passed
        {
            assert!(
                live_trials > 0,
                "seq {} passes before any live trial",
                event.seq
            );
        }
    }
    assert!(facts > 0, "no carried trial or failure facts");
    let phases = first_phases(&events);
    assert!(!phases.is_empty());
    for (core, phase) in &phases {
        assert_eq!(phase.to, CorePhase::Search, "{core:?}");
    }
    assert!(carried.failure_points && !carried.carried.is_empty());
    let (mut discriminating, mut points) = (false, 0);
    for seed in &carried.carried {
        let phase = &phases[&seed.core];
        let limit = seed.candidate_solo_limit.expect("a carried solo limit");
        let want = carried_start(
            limit.offset.get(),
            seed.failure_point.map(|point| point.offset.get()),
        );
        points += usize::from(seed.failure_point.is_some());
        assert_eq!(phase.to, CorePhase::Search);
        assert!(phase.check_solo_limit, "{:?}", seed.core);
        assert_eq!(phase.offset.get(), want, "{:?}", seed.core);
        assert_eq!(phase.pass, None, "{:?}", seed.core);
        assert_eq!(
            phase.failure_point,
            seed.failure_point.map(|point| point.offset)
        );
        let (decided, pass) = decisions[&seed.core];
        discriminating |= decided.get() != want || pass.is_some();
    }
    assert!(points > 0);
    assert!(
        discriminating,
        "every source decision equals its carried start"
    );
}

#[test]
fn a_ruleset_transition_starts_phase_one_with_nothing_confirmed() {
    let source = clean_cycle();
    let confirmed = togi_tuner::replay(&source.events()).unwrap().project();
    assert!(
        confirmed
            .bios_profile
            .as_ref()
            .is_some_and(|profile| profile.confirmed.is_some())
    );
    assert!(confirmed.clean_cycles > 0);
    let next = source.resumed(&shared_voltage(), SEED);
    let mut input = next_input(&next);
    input.observer = Some(after_initial_phases(16));
    finish(input);
    let state = togi_tuner::replay(&next.events()).unwrap().project();
    let plan = state.phase_plan.expect("a phase plan");
    assert_eq!(plan.phase, PhaseNo::One);
    assert_eq!(plan.concluded, None);
    assert!(!plan.confirming);
    assert!(plan.candidates.is_empty());
    assert!(
        state
            .bios_profile
            .as_ref()
            .is_none_or(|profile| profile.confirmed.is_none())
    );
    assert_eq!(state.clean_cycles, 0);
    assert_eq!(state.checking.cycle, None);
}

#[test]
fn a_ruleset_transition_after_a_bios_change_carries_only_solo_limits() {
    let source = clean_cycle();
    let recorded = source
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::SessionContext(context) = &event.payload {
                Some(context.bios_context())
            } else {
                None
            }
        })
        .unwrap();
    let mismatch = recorded.first_difference(&changed_bios()).unwrap();
    assert_eq!(mismatch.field, ContextField::BiosVersion);
    let next = source.resumed(&shared_voltage(), SEED);
    next.sim.set_bios_context(changed_bios());
    let mut input = next_input(&next);
    input.observer = Some(after_initial_phases(16));
    finish(input);
    let events = next.events();
    let carried = carried(&events);
    assert!(!carried.failure_points);
    assert_eq!(carried.detail, Some(mismatch.to_string()));
    assert!(!carried.carried.is_empty());
    assert!(
        carried
            .carried
            .iter()
            .all(|seed| seed.failure_point.is_none() && seed.candidate_solo_limit.is_some())
    );
    let phases = first_phases(&events);
    assert_eq!(phases.len(), 16);
    for (core, phase) in &phases {
        assert_eq!(phase.failure_point, None, "{core:?}");
    }
}

#[test]
fn a_bios_change_archives_and_checks_solo_limits() {
    let bench = Bench::new(&hunt_machine(4), 1);
    let mut input = bench.input();
    input.config = quick_matrix();
    input.observer = Some(after_alone_passes(4));
    finish(input);
    let id = session_of(&bench.events());

    bench.sim.set_bios_context(changed_bios());
    bench.sim.reboot();
    let mut input = bench.input();
    input.config = quick_matrix();
    input.observer = Some(after_initial_phases(4));
    finish(input);

    assert!(archived(&bench, id).is_some());
    let events = bench.events();
    assert_ne!(session_of(&events), id);
    let carried = carried(&events);
    assert!(!carried.failure_points);
    assert_eq!(carried.sources[0].session, id);
    assert_eq!(carried.carried.len(), 4);
    let phases = first_phases(&events);
    assert_eq!(phases.len(), 4);
    for (core, phase) in &phases {
        assert_eq!(phase.to, CorePhase::Search, "{core:?}");
        assert!(phase.check_solo_limit, "{core:?}");
    }
}

#[test]
fn a_configured_candidate_solo_limit_stops_short_of_a_carried_failure_point() {
    let source = clean_cycle();
    let next = source.resumed(&shared_voltage(), SEED);
    let mut input = next_input(&next);
    input.config = config(r#"{"candidate_solo_limits": {"0": -50}}"#);
    input.observer = Some(after_initial_phases(16));
    finish(input);
    let events = next.events();
    let point = carried_core(&events, core(0))
        .failure_point
        .expect("core 0 carries a failure point")
        .offset
        .get();
    assert!(point > -50, "{point}");
    let phase = &first_phases(&events)[&core(0)];
    assert_eq!(phase.to, CorePhase::Search);
    assert!(phase.check_solo_limit);
    assert_eq!(phase.offset.get(), point + 1);
    assert!(
        phase.reason.starts_with(&format!(
            "configured candidate solo limit; clamped to {}",
            point + 1
        )),
        "{}",
        phase.reason
    );
}

#[test]
fn a_carried_failure_point_at_zero_dead_ends_the_core() {
    let mut failing = hunt_machine(2);
    failing.core[0].alone = [1; 5];
    let source = Bench::new(&failing, 1);
    let first = finish(source.input());
    assert!(
        matches!(first.stop, Some(Stop::DeadEnd { .. })),
        "{:?}",
        first.stop
    );
    let id = session_of(&source.events());

    let next = source.resumed(&hunt_machine(2), 1);
    let finished = finish(next_input(&next));
    let Some(Stop::DeadEnd { dead_end, .. }) = finished.stop else {
        panic!("stopped with {:?}", finished.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::FailureAtZero);
    assert_eq!(dead_end.core, Some(core(0)));
    let events = next.events();
    let point = carried_core(&events, core(0))
        .failure_point
        .expect("core 0 carries a failure point");
    assert_eq!(point.offset.get(), 0);
    assert_eq!(point.from.session, id);
    let live_alone_on_core_zero = events.iter().any(|event| {
        matches!(&event.payload, Payload::TrialIntent(intent)
            if intent.core == Some(core(0)) && intent.condition == Condition::Alone)
    });
    assert!(!live_alone_on_core_zero);
}

#[test]
fn a_bios_archive_interrupted_before_the_move_resumes_the_carry() {
    let bench = Bench::new(&hunt_machine(4), 1);
    let mut input = bench.input();
    input.config = quick_matrix();
    input.observer = Some(after_alone_passes(4));
    finish(input);
    let id = session_of(&bench.events());

    bench.sim.set_bios_context(changed_bios());
    bench.sim.reboot();
    bench
        .fs
        .inject(Fault::before(Op::Rename, 1).named("events.jsonl"));
    let mut input = bench.input();
    input.config = quick_matrix();
    assert!(matches!(
        simulate(input),
        Err(SimError::Prepare {
            source: PrepareError::Archive(_),
            ..
        })
    ));
    bench.fs.clear_faults();

    let interrupted = bench.events();
    assert_eq!(session_of(&interrupted), id);
    assert!(matches!(
        &interrupted.last().unwrap().payload,
        Payload::SessionArchived(archive) if archive.session == id
    ));
    assert!(archived(&bench, id).is_none());

    let mut input = bench.input();
    input.config = quick_matrix();
    input.observer = Some(until_kind(EventKind::SessionCarried));
    finish(input);
    let events = bench.events();
    assert_ne!(session_of(&events), id);
    let carried = carried(&events);
    assert!(!carried.failure_points);
    assert!(!carried.carried.is_empty());
    assert_eq!(carried.sources[0].session, id);
    assert!(archived(&bench, id).is_some());
}

fn is_hunt_end_on_core_one(event: &Event, result: HuntOutcome) -> bool {
    matches!(
        &event.payload,
        Payload::HuntEnd(end)
            if end.result == result && end.cores.len() == 1 && end.cores.first() == Some(core(1))
    )
}

fn culprit_machine(joint: bool) -> MachineDefinition {
    let mut definition = hunt_machine(4);
    for limits in &mut definition.core {
        limits.alone = [-50; 5];
        limits.together = [-50; 7];
    }
    if joint {
        definition.joint.push(JointDef {
            members: BTreeMap::from([(1, -10)]),
            regimes: vec![Regime::R6],
            rate: Some(10.0),
            ..JointDef::default()
        });
    }
    definition
}

fn direct_machine() -> MachineDefinition {
    let mut definition = hunt_machine(4);
    definition.core[1].together[5] = -5;
    definition
}

fn transition_carrying_core_one(
    source: &Bench,
    definition: &MachineDefinition,
    config: Config,
) -> CarriedCore {
    let id = session_of(&source.events());
    let next = source.resumed(definition, 1);
    let mut input = next_input(&next);
    input.config = config;
    input.observer = Some(until_kind(EventKind::SessionCarried));
    finish(input);
    let events = next.events();
    let carried = carried(&events);
    assert!(carried.failure_points);
    assert_eq!(carried.sources[0].session, id);
    carried_core(&events, core(1))
}

/// The ended hunt on core 1: the end's sequence, the offset core 1 failed at when the hunt began,
/// and the offset the end itself names for core 1, if it names one.
struct HuntOfCoreOne {
    end: togi_journal::Seq,
    started: i64,
    named: Option<i64>,
}

fn hunt_of_core_one(events: &[Event], result: HuntOutcome) -> HuntOfCoreOne {
    let (seq, end) = events
        .iter()
        .find_map(|event| {
            if let Payload::HuntEnd(end) = &event.payload
                && is_hunt_end_on_core_one(event, result)
            {
                Some((event.seq, end))
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no {result} hunt on core 1"));
    let start = events
        .iter()
        .find_map(|event| {
            if let Payload::HuntStart(start) = &event.payload
                && start.hunt == end.hunt
            {
                Some(start)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no hunt start for {}", end.hunt));
    HuntOfCoreOne {
        end: seq,
        started: start.failing.get(core(1)).unwrap().get(),
        named: end
            .members
            .iter()
            .find(|member| member.core == core(1))
            .map(|member| member.offset.get()),
    }
}

fn culprit_source(definition_after: &MachineDefinition, config: &Config) -> Bench {
    let source = Bench::new(&culprit_machine(false), 1);
    let mut input = source.input();
    input.config = config.clone();
    input.policy.cycles = None;
    input.observer = Some(stop_when(phase_two_concludes(&source)));
    finish(input);

    let source = source.resumed(definition_after, 1);
    let mut input = source.input();
    input.config = config.clone();
    input.policy.cycles = None;
    input.observer = Some(stop_when(|event| {
        is_hunt_end_on_core_one(event, HuntOutcome::Culprit)
    }));
    finish(input);
    source
}

#[test]
fn a_ruleset_transition_carries_a_culprit_hunt_failure_point() {
    let config = candidate_config(-10, 4);
    let joint = culprit_machine(true);
    let source = culprit_source(&joint, &config);

    // This fixture's hunt locates the culprit without a live failed singleton, so its end may
    // name no member and the carry falls back to the `hunt.start` offset; a probed end's member
    // wins (pinned in togi-carry's seeds tests with a -30 start and a -23 member).
    let hunt = hunt_of_core_one(&source.events(), HuntOutcome::Culprit);
    let want = hunt.named.unwrap_or(hunt.started);
    let seed = transition_carrying_core_one(&source, &joint, config);
    let point = seed.failure_point.expect("a carried failure point");
    assert_eq!(point.offset.get(), want);
    assert_eq!(point.from.seq, hunt.end);
    assert_eq!(point.signal, Signal::Crash);
}

/// Transitions `source` under a candidate solo limit of -50 for every core and checks that core 1
/// carries `point` from the hunt end `end` and starts one count shallower than it.
fn assert_deep_limit_clamps_past(
    source: &Bench,
    definition: &MachineDefinition,
    point: i64,
    end: togi_journal::Seq,
) {
    let next = source.resumed(definition, 1);
    let mut input = next_input(&next);
    input.config = candidate_config(-50, 4);
    input.observer = Some(after_initial_phases(4));
    finish(input);
    let events = next.events();
    let carried = carried_core(&events, core(1))
        .failure_point
        .unwrap_or_else(|| panic!("core 1 carries a failure point"));
    assert_eq!(carried.offset.get(), point);
    assert_eq!(carried.from.seq, end);
    let phase = &first_phases(&events)[&core(1)];
    assert_eq!(phase.to, CorePhase::Search);
    assert_eq!(phase.offset.get(), point + 1);
    assert!(
        phase.reason.starts_with(&format!(
            "configured candidate solo limit; clamped to {}",
            point + 1
        )),
        "{}",
        phase.reason
    );
}

#[test]
fn a_transition_clamps_a_deep_candidate_limit_past_the_carried_culprit_point() {
    let joint = culprit_machine(true);
    let source = culprit_source(&joint, &candidate_config(-10, 4));
    let hunt = hunt_of_core_one(&source.events(), HuntOutcome::Culprit);
    let point = hunt.named.unwrap_or(hunt.started);
    assert_deep_limit_clamps_past(&source, &joint, point, hunt.end);
}

/// The simulated culprit hunt never reproduces a live singleton, so its end names no member. Take
/// that real journal, which stops at the culprit `hunt.end` before any backoff, and record the
/// end a probed singleton produces: core 1 failed at -30 when the hunt began and its shallowest
/// reproduced failure is -23 (the tuner's locate tests pin the producer). The transition must
/// carry -23 from the end and start a -50 candidate limit at -22, not at the -29 the start gives.
#[test]
fn a_transition_starts_past_the_refined_member_of_a_recorded_singleton_end() {
    let joint = culprit_machine(true);
    let source = culprit_source(&joint, &candidate_config(-10, 4));
    let mut events = source.events();
    let ended = hunt_of_core_one(&events, HuntOutcome::Culprit);
    // The run stops after the end plus the events it appended around it; cut the journal back
    // to the end itself, as if the machine went down before the culprit backoff was decided.
    events.retain(|event| event.seq <= ended.end);
    assert_eq!(
        events.last().map(|event| event.seq),
        Some(ended.end),
        "the journal stops at the hunt end, before the backoff"
    );
    assert!(
        !events.iter().any(|event| event.seq > ended.end
            || event.kind() == EventKind::TunerDecision && event.cause.contains(&ended.end)),
        "no decision answers the hunt end"
    );
    for event in &mut events {
        if let Payload::HuntStart(start) = &mut event.payload
            && event.seq < ended.end
        {
            if start.failing.get(core(1)).is_some() {
                start.failing = start
                    .failing
                    .iter()
                    .map(|(id, at)| (id, if id == core(1) { offset(-30) } else { at }))
                    .collect();
            }
        } else if let Payload::HuntEnd(end) = &mut event.payload
            && event.seq == ended.end
        {
            end.members = vec![togi_journal::Member {
                core: core(1),
                offset: offset(-23),
            }];
        }
    }
    let mut text = String::new();
    for event in &events {
        text.push_str(&event.encode().unwrap());
        text.push('\n');
    }
    source
        .fs
        .put(format!("{STATE}/events.jsonl"), text.into_bytes());
    let rewritten = hunt_of_core_one(&source.events(), HuntOutcome::Culprit);
    assert_eq!((rewritten.started, rewritten.named), (-30, Some(-23)));

    assert_deep_limit_clamps_past(&source, &joint, -23, ended.end);
}

#[test]
fn a_ruleset_transition_carries_a_direct_hunt_failure_point() {
    let config = candidate_config(-10, 4);
    let definition = direct_machine();
    let source = Bench::new(&definition, 1);
    let mut input = source.input();
    input.config = config.clone();
    input.policy.cycles = None;
    input.observer = Some(stop_when(|event| {
        is_hunt_end_on_core_one(event, HuntOutcome::Direct)
    }));
    finish(input);

    let events = source.events();
    let want = hunt_of_core_one(&events, HuntOutcome::Direct).started;
    let (failure_seq, signal) = events
        .iter()
        .find_map(|event| {
            if let Payload::Failure(failure) = &event.payload
                && failure.core == Some(core(1))
                && failure.offset.map(TargetOffset::get) == Some(want)
                && failure.attribution == Attribution::Attributed
                && failure.known_failure.is_none()
            {
                Some((event.seq, failure.signal))
            } else {
                None
            }
        })
        .expect("a failure at the hunt's offset");
    let seed = transition_carrying_core_one(&source, &definition, config);
    let point = seed.failure_point.expect("a carried failure point");
    assert_eq!(point.offset.get(), want);
    assert_eq!(point.from.seq, failure_seq);
    assert_eq!(point.signal, signal);
}

/// What makes two passes evidence of the same trial class.
#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ClassKey {
    regime: Regime,
    workload: WorkloadId,
    cores: Vec<CoreId>,
    duration_s: u32,
}

impl ClassKey {
    fn live(intent: &TrialIntent) -> Self {
        Self {
            regime: intent.regime,
            workload: intent.workload,
            cores: togi_tuner::Load::recorded(intent.core, intent.cores)
                .cores()
                .iter()
                .collect(),
            duration_s: intent.duration_s,
        }
    }

    fn carried(class: &TrialClass) -> Self {
        Self {
            regime: class.regime,
            workload: class
                .workload
                .unwrap_or_else(|| panic!("a carried pass names no workload: {class:?}")),
            cores: class.cores.iter().collect(),
            duration_s: class.duration_s,
        }
    }
}

fn first_cycle_live_passes(events: &[Event]) -> BTreeMap<ClassKey, usize> {
    let mut intents: HashMap<TrialId, &TrialIntent> = HashMap::new();
    let mut counts = BTreeMap::new();
    let mut open = false;
    for event in events {
        let payload = &event.payload;
        if let Payload::CheckingCycle(cycle) = payload
            && cycle.event == CycleEvent::Start
        {
            assert!(!open, "first cycle restarted before passing in full");
            open = true;
        } else if let Payload::CheckingCycle(cycle) = payload
            && open
        {
            assert!(cycle.passed && cycle.full, "{cycle:?}");
            return counts;
        } else if let Payload::TrialIntent(intent) = payload
            && open
        {
            intents.insert(intent.trial.clone(), intent);
        } else if let Payload::TrialEnd(end) = payload
            && let Some(intent) = intents.get(&end.trial)
        {
            assert_eq!(end.outcome, Outcome::Pass, "{end:?}");
            *counts.entry(ClassKey::live(intent)).or_default() += 1;
        }
    }
    panic!("no first passed full cycle");
}

#[test]
fn a_transition_checks_carried_solo_limits_but_completes_full_cycles_with_live_passes() {
    let definition = hunt_machine(2);
    let config = candidate_config(-10, 2);
    let source = Bench::new(&definition, 1);
    let mut input = source.input();
    input.config = config.clone();
    input.policy.cycles = None;
    input.observer = Some(stop_when(is_passed_full_cycle));
    finish(input);
    let source_events = source.events();
    let id = session_of(&source_events);

    let next = source.resumed(&definition, 1);
    let mut input = next_input(&next);
    input.config = config.clone();
    input.policy.cycles = None;
    input.observer = Some(stop_when(is_passed_full_cycle));
    finish(input);
    let events = next.events();

    let Payload::SessionStart(start) = &events[0].payload else {
        panic!("first event is {:?}", events[0].payload);
    };
    assert_eq!(start.build, next_build());
    assert!(archived(&next, id).is_some());

    let trials = config.evidence().trials().get();
    let mut facts = HashMap::new();
    let mut together_passes: BTreeMap<ClassKey, usize> = BTreeMap::new();
    let mut completed = BTreeSet::new();
    for event in &events {
        let payload = &event.payload;
        if let Payload::TrialCarried(fact) = payload {
            if fact.source.session == id
                && fact.condition == Condition::Together
                && fact.outcome == Outcome::Pass
            {
                *together_passes
                    .entry(ClassKey::carried(&fact.class))
                    .or_default() += 1;
            }
            facts.insert(event.seq, fact);
        } else if let Payload::TrialIntent(intent) = payload {
            assert_ne!(
                intent.activity,
                Some(Activity::Search),
                "live solo limit trial at seq {}",
                event.seq
            );
        } else if let Payload::CorePhase(phase) = payload
            && phase.from == Some(CorePhase::Search)
            && phase.to != CorePhase::Search
        {
            let mut counts: HashMap<Regime, u32> = HashMap::new();
            for cause in &event.cause {
                if let Some(fact) = facts.get(cause)
                    && fact.source.session == id
                    && fact.outcome == Outcome::Pass
                    && fact.class.cores.len() == 1
                    && fact.class.cores.first() == Some(phase.core)
                {
                    *counts.entry(fact.class.regime).or_default() += 1;
                }
            }
            for regime in [Regime::R1, Regime::R2] {
                assert_eq!(
                    counts.get(&regime).copied().unwrap_or(0),
                    trials,
                    "{:?} {regime:?}",
                    phase.core
                );
            }
            completed.insert(phase.core);
        }
    }
    assert_eq!(completed, BTreeSet::from([core(0), core(1)]));
    let source_passes = first_cycle_live_passes(&source_events);
    assert!(!source_passes.is_empty());
    assert_eq!(source_passes, together_passes);
    assert_eq!(source_passes, first_cycle_live_passes(&events));
}

#[test]
fn a_transition_starts_a_hunt_from_a_carried_together_failure() {
    let mut definition = hunt_machine(4);
    for limits in &mut definition.core {
        limits.alone = [-11; 5];
        limits.together = [-11, -11, -11, -11, -11, -11, -50];
    }
    definition.core[1].together[5] = -5;
    let source = Bench::new(&definition, 1);
    let mut input = source.input();
    input.config = candidate_config(-10, 4);
    input.policy.cycles = None;
    input.observer = Some(stop_when(|event| {
        matches!(
            &event.payload,
            Payload::Failure(failure)
                if failure.condition == Some(Condition::Together)
                    && failure.regime == Some(Regime::R6)
                    && failure.attribution == Attribution::Unattributed
        )
    }));
    finish(input);

    let source_events = source.events();
    let id = session_of(&source_events);
    let failure = source_events
        .iter()
        .rev()
        .find_map(|event| {
            if let Payload::Failure(failure) = &event.payload {
                Some(failure.clone())
            } else {
                None
            }
        })
        .expect("a checking failure together");
    let trial = failure.trial.expect("a failing trial");
    let intent = source_events
        .iter()
        .find_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload
                && intent.trial == trial
            {
                Some(intent.clone())
            } else {
                None
            }
        })
        .expect("the failing trial's intent");
    assert_eq!(intent.activity, Some(Activity::Checking));
    assert_eq!(intent.profile, uniform(-9, 4));

    let next = source.resumed(&definition, 1);
    let mut input = next_input(&next);
    input.config = candidate_config(-11, 4);
    input.policy.cycles = None;
    input.observer = Some(stop_when(|event| {
        matches!(event.payload, Payload::HuntStart(_))
    }));
    finish(input);
    let events = next.events();
    let Payload::SessionStart(start) = &events[0].payload else {
        panic!("first event is {:?}", events[0].payload);
    };
    assert_eq!(start.build, next_build());

    let mut carried_at = None;
    let mut current_profile = None;
    let mut hunts = 0;
    for event in &events {
        let payload = &event.payload;
        if let Payload::ProfileChange(change) = payload {
            current_profile = Some(change.to);
        } else if let Payload::TrialCarried(fact) = payload
            && fact.source.session == id
            && fact.source.trial.as_ref() == Some(&trial)
            && fact.outcome == Outcome::Failure
        {
            carried_at = Some(event.seq);
        } else if let Payload::TrialIntent(rerun) = payload
            && rerun.condition == Condition::Together
            && rerun.regime == intent.regime
            && rerun.workload == intent.workload
            && rerun.duration_s == intent.duration_s
            && rerun.cores == intent.cores
            && rerun.profile == intent.profile
        {
            panic!("the carried failure was rerun: {rerun:?}");
        } else if let Payload::HuntStart(hunt) = payload {
            hunts += 1;
            assert_eq!(current_profile, Some(uniform(-10, 4)));
            assert_eq!(Some(hunt.failure), carried_at);
            assert_eq!(hunt.trial.as_ref(), Some(&trial));
            assert_eq!(hunt.failing, intent.profile);
            assert_eq!(hunt.regime, intent.regime);
            assert_eq!(hunt.workload, intent.workload);
            assert_eq!(hunt.duration_s, intent.duration_s);
            assert_eq!(hunt.cores, intent.cores);
        }
    }
    assert_eq!(hunts, 1);
}

fn journal_with_unknown_kind() -> (Bench, SessionId, Vec<u8>) {
    let bench = Bench::new(&hunt_machine(2), 1);
    let mut input = bench.input();
    input.observer = Some(after_alone_passes(1));
    finish(input);
    let events = bench.events();
    let id = session_of(&events);
    let path = format!("{STATE}/events.jsonl");
    let mut text = String::from_utf8(bench.fs.contents(&path).unwrap()).unwrap();
    let mut line: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    line["seq"] = (events.last().unwrap().seq.get() + 1).into();
    line["kind"] = "retired.fact".into();
    line["msg"] = "unknown evidence".into();
    line["data"] = serde_json::json!({"core": 0, "offset": -50});
    line.as_object_mut().unwrap().remove("cause");
    text.push_str(&line.to_string());
    text.push('\n');
    bench.fs.put(&path, text.as_bytes());
    (bench, id, text.into_bytes())
}

#[test]
fn a_journal_with_an_unknown_kind_is_refused_untouched_by_the_build_that_wrote_it() {
    let (bench, id, original) = journal_with_unknown_kind();
    let error = simulate(bench.input()).err().expect("a refused journal");
    assert!(
        matches!(
            error,
            SimError::Prepare {
                source: PrepareError::Open(_),
                ..
            }
        ),
        "{error}"
    );
    let lines = std::str::from_utf8(&original).unwrap().lines().count();
    assert_eq!(
        togi_machine::error_chain(&error),
        format!(
            "prepare the journal for boot 1: open the journal: {STATE}/events.jsonl: line {lines}: unknown event kind \"retired.fact\""
        )
    );
    assert_eq!(
        bench.fs.contents(format!("{STATE}/events.jsonl")),
        Some(original)
    );
    assert!(archived(&bench, id).is_none());
}

#[test]
fn an_older_journal_with_an_unknown_kind_is_archived_and_carries_nothing() {
    let (bench, id, original) = journal_with_unknown_kind();
    let run = || {
        let mut input = next_input(&bench);
        input.observer = Some(until_kind(EventKind::TrialEnd));
        let finished = finish(input);
        assert!(finished.errors.is_empty(), "{:?}", finished.errors);
        assert_eq!(finished.stop, Some(Stop::Signal));
        bench.events()
    };
    let first = run();
    assert_eq!(archived(&bench, id), Some(original));
    let session = session_of(&first);
    assert_ne!(session, id);
    let carried: Vec<_> = first
        .iter()
        .filter_map(|event| {
            if let Payload::SessionCarried(carried) = &event.payload {
                Some(carried)
            } else {
                None
            }
        })
        .collect();
    assert!(
        carried.iter().all(|carried| carried.carried.is_empty()),
        "{carried:?}"
    );

    let next = run();
    assert_eq!(session_of(&next), session);
    assert!(next.len() > first.len());
    let ends = |events: &[Event]| {
        events
            .iter()
            .filter(|event| event.kind() == EventKind::TrialEnd)
            .count()
    };
    assert_eq!(ends(&next), ends(&first) + 1);
}
