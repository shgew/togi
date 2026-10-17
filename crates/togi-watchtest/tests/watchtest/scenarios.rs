use togi_journal::{
    Activity, DeadEndCondition, Event, EventKind, Outcome, Payload, ShutdownReason,
};
use togi_machine::{Regime, Signal};
use togi_tuner::{PhaseNo, State};
use togi_watchtest::{Base, Scenario};

use crate::support::{fold, intent_of, running_trial, states_at};

fn cut(scenario: Scenario) -> Vec<Event> {
    scenario.events().expect("the scenario builds")
}

fn prefix_of(base: &[Event], cut: &[Event]) {
    assert!(
        base.starts_with(cut),
        "the cut is a prefix of its base simulation"
    );
}

#[test]
fn every_scenario_names_a_distinct_golden_and_builds() {
    let names: Vec<&str> = Scenario::ALL
        .iter()
        .map(|scenario| scenario.name())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 18, "{names:?}");
    for want in [
        "search",
        "checking",
        "hunt",
        "member-probe",
        "round",
        "confirmation",
        "concluded",
        "unconfirmed",
        "hunt-bios",
        "idle",
        "crashed",
        "recovering",
        "combination",
        "between",
        "confirm",
        "deadend",
        "first-result",
        "stopped",
    ] {
        assert!(names.contains(&want), "{want} is missing from {names:?}");
    }
}

#[test]
fn the_default_session_scenarios_reach_the_state_their_names_claim() {
    let base = Base::Session.events().expect("the base simulates");
    let scenarios = [
        Scenario::Search,
        Scenario::Checking,
        Scenario::Hunt,
        Scenario::Confirmation,
        Scenario::Idle,
        Scenario::Crashed,
        Scenario::Recovering,
        Scenario::Between,
        Scenario::Stopped,
    ];
    let cuts: Vec<Vec<Event>> = scenarios.iter().map(|scenario| cut(*scenario)).collect();
    for cut in &cuts {
        prefix_of(base, cut);
    }
    let lengths: Vec<usize> = cuts.iter().map(Vec::len).collect();
    let states = states_at(base, &lengths);
    let pick = |scenario: Scenario| {
        let at = scenarios
            .iter()
            .position(|s| *s == scenario)
            .expect("listed");
        (&cuts[at], &states[at])
    };

    let (events, state) = pick(Scenario::Search);
    let plan = state.phase_plan().expect("the session started");
    let intent = running_trial(events);
    assert_eq!(intent.activity, Some(Activity::Search));
    assert!(intent.offset.is_some_and(|offset| offset.get() < 0));
    assert_eq!(plan.phase, PhaseNo::One);
    assert!(state.bios_profile().expect("started").confirmed.is_none());

    let (events, state) = pick(Scenario::Checking);
    let intent = running_trial(events);
    assert_eq!(intent.activity, Some(Activity::Checking));
    assert_eq!(intent.regime, Regime::R7);
    assert!(intent.cycle.is_some() && !intent.rerun);
    assert_eq!(state.phase_plan().expect("started").phase, PhaseNo::One);

    let (events, state) = pick(Scenario::Hunt);
    let intent = running_trial(events);
    assert_eq!(intent.activity, Some(Activity::Hunt));
    assert!(intent.hunt.is_some());
    assert!(state.hunt_plan().is_some(), "a hunt is open");

    let (events, state) = pick(Scenario::Confirmation);
    running_trial(events);
    let plan = state.phase_plan().expect("started");
    assert_eq!(plan.phase, PhaseNo::Two);
    assert!(plan.confirming);

    let (events, _) = pick(Scenario::Idle);
    let intent = running_trial(events);
    assert_eq!(intent.regime, Regime::R6);
    assert_eq!(intent.activity, Some(Activity::Checking));

    let (events, _) = pick(Scenario::Between);
    let Payload::TrialEnd(end) = &events.last().expect("events").payload else {
        panic!("the journal ends on {:?}", events.last().map(Event::kind));
    };
    assert_eq!(end.outcome, Outcome::Pass);
    let passed = intent_of(events, &end.trial);
    assert_eq!(passed.activity, Some(Activity::Checking));
    assert!(passed.cycle.is_some() && !passed.rerun);
    assert!(
        togi_journal::State::replay(events)
            .expect("the journal folds")
            .in_flight()
            .is_none(),
        "no trial is open between trials"
    );

    let (events, _) = pick(Scenario::Stopped);
    assert!(matches!(
        &events.last().expect("events").payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Cycles
    ));
    let (_, state) = pick(Scenario::Stopped);
    assert_eq!(
        state.phase_plan().expect("started").phase,
        PhaseNo::Concluded
    );
    assert!(state.clean_cycles() >= 1);

    let (crashed, _) = pick(Scenario::Crashed);
    let (recovering, _) = pick(Scenario::Recovering);
    assert!(
        crashed
            .iter()
            .all(|event| event.kind() != EventKind::CrashDetected)
    );
    let last_start = crashed
        .iter()
        .rev()
        .find_map(|event| {
            if let Payload::TrialStart(start) = &event.payload {
                Some((event, start))
            } else {
                None
            }
        })
        .expect("a trial started");
    assert!(
        !crashed.iter().any(|event| matches!(&event.payload, Payload::TrialEnd(end) if end.trial == last_start.1.trial)),
        "the trial was outlived by the machine"
    );
    assert_ne!(
        crashed.last().expect("events").boot,
        last_start.0.boot,
        "a later boot has begun"
    );
    assert_eq!(recovering.len(), crashed.len() + 1);
    let Payload::CrashDetected(crash) = &recovering.last().expect("events").payload else {
        panic!("recovering ends on the crash record");
    };
    assert_eq!(crash.previous_boot, last_start.0.boot);
    assert!(crash.in_flight.is_some());
}

#[test]
fn the_dead_end_scenario_ends_on_a_dead_end_the_runner_cited_after_the_trial_ended() {
    let events = cut(Scenario::Deadend);
    let (dead, before) = events.split_last().expect("events");
    let Payload::DeadEnd(dead_end) = &dead.payload else {
        panic!("the journal ends on {:?}", dead.kind());
    };
    assert_eq!(dead_end.condition, DeadEndCondition::NoEvidence);
    assert!(
        togi_journal::State::replay(&events)
            .expect("the journal folds")
            .in_flight()
            .is_none(),
        "no trial is open beside the dead end"
    );
    assert!(!dead.cause.is_empty(), "the dead end cites its evidence");
    for cause in &dead.cause {
        let cited = &before[usize::try_from(cause.get() - 1).expect("seq fits")];
        let Payload::TrialEnd(end) = &cited.payload else {
            panic!("the dead end cites {:?}", cited.kind());
        };
        assert_eq!(end.outcome, Outcome::Inconclusive);
        assert!(end.backend_missing);
    }
    let checked_r7 = before.iter().any(|event| {
        matches!(&event.payload, Payload::TrialEnd(end) if {
            let intent = intent_of(before, &end.trial);
            intent.activity == Some(Activity::Checking) && intent.regime == Regime::R7 && intent.cycle.is_some()
        })
    });
    assert!(
        checked_r7,
        "an R7 checking cycle trial ended before the dead end"
    );
    assert!(
        !before
            .iter()
            .any(|event| event.kind() == EventKind::DeadEnd)
    );
    fold(&events);
}

#[test]
fn the_probe_machine_scenarios_reach_the_state_their_names_claim() {
    let base = Base::Probe.events().expect("the base simulates");
    let confirm = cut(Scenario::Confirm);
    let member_probe = cut(Scenario::MemberProbe);
    let round = cut(Scenario::Round);
    for cut in [&confirm, &member_probe, &round] {
        prefix_of(base, cut);
    }
    let states = states_at(base, &[confirm.len(), member_probe.len(), round.len()]);

    let intent = running_trial(&confirm);
    assert_eq!(intent.activity, Some(Activity::Search));
    assert_eq!(
        intent.offset.map(togi_machine::TargetOffset::get),
        Some(-11),
        "the configured candidate solo limit"
    );
    assert_eq!(states[0].phase_plan().expect("started").phase, PhaseNo::One);
    assert!(
        !confirm
            .iter()
            .any(|event| event.kind() == EventKind::TrialEnd)
    );

    let intent = running_trial(&member_probe);
    assert_eq!(intent.activity, Some(Activity::Hunt));
    assert!(states[1].hunt_plan().is_some());
    let probing = member_probe
        .iter()
        .rev()
        .find_map(|event| {
            if let Payload::HuntGroup(group) = &event.payload {
                Some(group)
            } else {
                None
            }
        })
        .expect("a hunt group precedes the trial");
    assert!(
        probing.probe.is_some(),
        "the latest hunt group probes a member"
    );
    assert_eq!(Some(probing.group), intent.group);

    let intent = running_trial(&round);
    assert_eq!(intent.activity, Some(Activity::Deepening));
    assert!(intent.round.is_some());
    let plan = states[2].phase_plan().expect("started");
    assert_eq!(plan.phase, PhaseNo::Two);
    assert_eq!(plan.round, Some(1));
}

#[test]
fn the_combination_scenario_ends_on_the_probe_machines_found_combination() {
    let events = cut(Scenario::Combination);
    let found = events
        .iter()
        .find_map(|event| {
            if let Payload::Combination(combination) = &event.payload {
                Some(combination)
            } else {
                None
            }
        })
        .expect("the session recorded a combination");
    let cores: Vec<i64> = found
        .members
        .iter()
        .map(|member| i64::from(member.core.get()))
        .collect();
    assert_eq!(cores, [3, 11]);
    assert!(matches!(
        &events.last().expect("events").payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Cycles
    ));
}

#[test]
fn the_concluded_scenario_runs_checking_on_after_phase_2_concluded() {
    let events = cut(Scenario::Concluded);
    let intent = running_trial(&events);
    assert_eq!(intent.activity, Some(Activity::Checking));
    let state = fold(&events);
    let plan = state.phase_plan().expect("started");
    assert_eq!(plan.phase, PhaseNo::Concluded);
    assert!(plan.concluded.is_some());
    assert!(state.clean_cycles() >= 1);
    assert!(state.bios_profile().expect("started").confirmed.is_some());
}

#[test]
fn the_unconfirmed_scenario_shows_a_core_off_the_confirmed_bios_profile() {
    let events = cut(Scenario::Unconfirmed);
    running_trial(&events);
    let bios = fold(&events).bios_profile().expect("started");
    assert!(bios.confirmed.is_some());
    assert!(!bios.unconfirmed.is_empty());
}

#[test]
fn the_later_hunt_crashes_six_consecutive_trials_from_the_first_eligible_checking_trial() {
    let events = Base::LaterHunt.events().expect("the base simulates");
    let mut state = State::new();
    let start = events
        .iter()
        .position(|event| {
            state.fold(event).expect("the journal folds");
            matches!(&event.payload, Payload::TrialIntent(intent) if
                intent.activity == Some(Activity::Checking)
                    && intent.cycle.is_some_and(|cycle| cycle.get() >= 4)
                    && intent.regime != Regime::R7
                    && state.bios_profile().is_some_and(|bios| bios.confirmed.is_some())
            )
        })
        .expect("checking outside R7 runs after a confirmed BIOS profile, in cycle 4 or later");
    let trials: Vec<_> = events[start..]
        .iter()
        .filter_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload {
                Some(&intent.trial)
            } else {
                None
            }
        })
        .take(6)
        .collect();
    assert_eq!(trials.len(), 6);
    for trial in trials {
        let end = events[start..]
            .iter()
            .find_map(|event| {
                if let Payload::TrialEnd(end) = &event.payload
                    && &end.trial == trial
                {
                    Some(end)
                } else {
                    None
                }
            })
            .expect("each burst trial ended");
        assert_eq!(end.signal, Some(Signal::Crash), "trial {trial}");
        assert_eq!(end.core, None, "trial {trial} names no core");
    }
}

#[test]
fn the_hunt_bios_scenario_runs_a_hunt_beside_a_confirmed_profile_with_unconfirmed_cores() {
    let events = cut(Scenario::HuntBios);
    let intent = running_trial(&events);
    assert_eq!(intent.activity, Some(Activity::Hunt));
    let state = fold(&events);
    assert!(state.hunt_plan().is_some());
    let bios = state.bios_profile().expect("started");
    assert!(bios.confirmed.is_some());
    assert!(!bios.unconfirmed.is_empty());
}

#[test]
fn the_first_result_scenario_stops_at_the_first_confirmed_bios_profile() {
    let events = cut(Scenario::FirstResult);
    assert!(matches!(
        &events.last().expect("events").payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::FirstResult
    ));
    let state = fold(&events);
    assert!(state.first_result());
    assert!(state.bios_profile().expect("started").confirmed.is_some());
    assert_eq!(state.clean_cycles(), 0);
}
