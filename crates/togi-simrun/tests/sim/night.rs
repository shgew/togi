//! Ruleset 14 nights through the simulation: the real session over the simulated seams. Whole-night assertions
//! retain the operator's `stop_at`; transition assertions stop from the event observer.

use jiff::SignedDuration;
use test_case::test_case;
use togi_journal::{
    DeadEndCondition, Event, EventKind, Outcome, Payload, Queue, ShutdownReason, StockAttested,
    TrialIntent,
};
use togi_machine::{Backend, BiosContext, CoreId, Signal};
use togi_session::Stop;
use togi_sim::MachineDefinition;
use togi_sim::definition::ScriptDef;
use togi_simrun::{Observe, Replay};

use crate::common::{Bench, crash_at_nth, finish, hunt_machine};

fn intents(events: &[Event]) -> Vec<&TrialIntent> {
    events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload {
                Some(intent)
            } else {
                None
            }
        })
        .collect()
}

fn count(events: &[Event], kind: EventKind) -> usize {
    events.iter().filter(|event| event.kind() == kind).count()
}

fn shutdown_reasons(events: &[Event]) -> Vec<ShutdownReason> {
    events
        .iter()
        .filter_map(|event| {
            if let Payload::Shutdown(shutdown) = &event.payload {
                Some(shutdown.reason)
            } else {
                None
            }
        })
        .collect()
}

fn all_zero(bench: &Bench, cores: u32) {
    let mut smu = bench.sim.seams().smu;
    for n in 0..cores {
        let core = CoreId::new(i64::from(n)).unwrap();
        assert_eq!(smu.offset(core).unwrap().0, 0, "core {n}");
    }
}

fn attestations(events: &[Event]) -> Vec<&StockAttested> {
    events
        .iter()
        .filter_map(|event| {
            if let Payload::StockAttested(attested) = &event.payload {
                Some(attested)
            } else {
                None
            }
        })
        .collect()
}

/// No trial of a night runs every core at CO 0.
fn assert_no_zero_trial(events: &[Event]) {
    for intent in intents(events) {
        assert!(
            intent.profile.iter().any(|(_, offset)| offset.get() != 0),
            "trial {} ran all-zero",
            intent.trial.as_str()
        );
    }
}

#[test]
fn a_first_night_is_attested_once_and_maps_nonzero_until_the_operator_ends_it() {
    let bench = Bench::new(&MachineDefinition::default(), 1);
    let finished = finish(bench.input());

    assert_eq!(finished.stop, Some(Stop::Signal));
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    let events = bench.events();
    assert_eq!(events[0].kind(), EventKind::SessionStart);
    assert_eq!(events[1].kind(), EventKind::StockAttested);
    assert_eq!(count(&events, EventKind::StockAttested), 1);
    let queues: Vec<Queue> = intents(&events)
        .iter()
        .filter_map(|intent| intent.task.map(|task| task.queue))
        .collect();
    assert!(queues.contains(&Queue::Mapping), "{queues:?}");
    assert!(!queues.contains(&Queue::Control), "{queues:?}");
    assert_no_zero_trial(&events);
    assert_eq!(shutdown_reasons(&events), [ShutdownReason::Signal]);
    let nights: Vec<_> = intents(&events).iter().map(|intent| intent.night).collect();
    assert!(
        nights.windows(2).all(|pair| pair[0] == pair[1]),
        "{nights:?}"
    );
    all_zero(&bench, 8);
}

#[test]
fn a_changed_firmware_is_attested_again_and_an_unchanged_one_is_not() {
    let definition = MachineDefinition::default();
    let night = SignedDuration::from_mins(20);
    let first = Bench::new(&definition, 1);
    let mut input = first.input();
    input.stop_at = Some(first.sim.now() + night);
    finish(input);

    let second = first.resumed(&definition, 1);
    let mut input = second.input();
    input.stop_at = Some(second.sim.now() + night);
    finish(input);
    assert_eq!(attestations(&second.events()).len(), 1);

    let third = second.resumed(&definition, 1);
    let recorded = third.sim.seams().host.bios_context().unwrap();
    let changed = BiosContext {
        bios_version: "SIM.2".to_owned(),
        ..recorded.clone()
    };
    third.sim.set_bios_context(changed.clone());
    let mut input = third.input();
    input.stop_at = Some(third.sim.now() + night);
    let finished = finish(input);
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);

    let events = third.events();
    let attested: Vec<_> = attestations(&events)
        .into_iter()
        .map(|attested| &attested.firmware)
        .collect();
    assert_eq!(attested, [&recorded, &changed]);
    assert_no_zero_trial(&events);
}

#[test_case(Replay::Warm; "warm")]
#[test_case(Replay::Cold; "cold")]
fn a_crash_mid_night_is_recovered_and_the_same_night_continues(replay: Replay) {
    let bench = Bench::new(&MachineDefinition::default(), 1);
    let mut input = bench.input();
    input.observer = Some(crash_at_nth(&bench.sim, EventKind::TrialEnd, 4));
    input.replay = replay;
    let finished = finish(input);

    assert_eq!(finished.stop, Some(Stop::Signal));
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    let events = bench.events();
    assert!(count(&events, EventKind::CrashDetected) >= 1);
    assert_eq!(count(&events, EventKind::SessionStart), 1);
    let nights: Vec<_> = intents(&events).iter().map(|intent| intent.night).collect();
    assert!(nights.len() > 1, "{nights:?}");
    assert!(
        nights.windows(2).all(|pair| pair[0] == pair[1]),
        "the reboot opened another night: {nights:?}"
    );
    assert_eq!(
        shutdown_reasons(&events).last(),
        Some(&ShutdownReason::Signal)
    );
    all_zero(&bench, 8);
}

#[test]
fn an_operator_signal_ends_the_night_and_a_restart_opens_a_new_one() {
    let definition = MachineDefinition::default();
    let first = Bench::new(&definition, 1);
    let mut input = first.input();
    input.stop_at = Some(first.sim.now() + SignedDuration::from_mins(20));
    let finished = finish(input);
    assert_eq!(finished.stop, Some(Stop::Signal));
    let before = intents(&first.events()).len();
    assert!(before > 0);

    let second = first.resumed(&definition, 1);
    let mut input = second.input();
    input.stop_at = Some(second.sim.now() + SignedDuration::from_mins(20));
    let finished = finish(input);
    assert_eq!(finished.stop, Some(Stop::Signal));
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);

    let events = second.events();
    assert_eq!(
        shutdown_reasons(&events),
        [ShutdownReason::Signal, ShutdownReason::Signal]
    );
    let nights: Vec<_> = intents(&events).iter().map(|intent| intent.night).collect();
    assert!(nights.len() > before);
    let (earlier, later) = nights.split_at(before);
    assert!(
        earlier.windows(2).all(|pair| pair[0] == pair[1]),
        "{nights:?}"
    );
    assert!(
        later.windows(2).all(|pair| pair[0] == pair[1]),
        "{nights:?}"
    );
    assert_ne!(
        earlier[0], later[0],
        "the restart continued the closed night"
    );
    assert_eq!(count(&events, EventKind::SessionStart), 1);
}

#[test]
fn a_failure_on_the_first_mapping_trial_is_a_mapping_failure_not_a_stock_dead_end() {
    let mut definition = hunt_machine(4);
    definition.script = vec![ScriptDef {
        trial: "0001".into(),
        signal: Some(Signal::Crash),
        ..ScriptDef::default()
    }];
    let bench = Bench::new(&definition, 1);
    let finished = finish(bench.input());

    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    assert!(
        !matches!(
            &finished.stop,
            Some(Stop::DeadEnd { dead_end, .. }) if dead_end.condition == DeadEndCondition::FailureAtZero
        ),
        "{:?}",
        finished.stop
    );
    let events = bench.events();
    let failure = events
        .iter()
        .position(|event| matches!(&event.payload, Payload::Failure(failure) if failure.signal == Signal::Crash))
        .expect("the crash has no failure");
    assert!(
        events[failure..]
            .iter()
            .any(|event| event.kind() == EventKind::TrialIntent),
        "the night stopped at the first failure"
    );
    assert_no_zero_trial(&events);
    all_zero(&bench, 4);
}

fn ran_on(events: &[Event], backend: Backend) -> usize {
    intents(events)
        .iter()
        .filter(|intent| {
            intent
                .content
                .iter()
                .any(|content| content.content.backend == backend)
        })
        .count()
}

#[test]
fn an_exhausted_backend_is_left_and_the_night_continues_on_the_other() {
    let bench = Bench::new(&MachineDefinition::default(), 1);
    let sim = bench.sim.clone();
    let mut mapping = false;
    let mut failed = false;
    let mut excluded = false;
    let mut input = bench.input();
    // The backend fails after the first mapping trial.
    input.observer = Some(Box::new(move |event| {
        if let Payload::NightPlan(plan) = &event.payload {
            excluded = plan
                .unavailable
                .iter()
                .any(|unavailable| unavailable.backend == Backend::Mprime);
        }
        if let Payload::TrialIntent(intent) = &event.payload
            && intent.task.is_some_and(|task| task.queue == Queue::Mapping)
        {
            mapping = true;
        } else if matches!(event.payload, Payload::TrialEnd(_)) && mapping && !failed {
            failed = true;
            sim.fail_backend_setup(Backend::Mprime);
        } else if event.kind() == EventKind::TrialEnd && excluded {
            return Observe::Stop;
        }
        Observe::Continue
    }));
    let finished = finish(input);

    assert_eq!(finished.stop, Some(Stop::Signal), "{:?}", finished.errors);
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    let events = bench.events();
    let last_mprime = intents(&events)
        .iter()
        .rposition(|intent| {
            intent
                .content
                .iter()
                .any(|content| content.content.backend == Backend::Mprime)
        })
        .expect("the night never tried the backend that failed");
    let later = &intents(&events)[last_mprime + 1..];
    assert!(
        later.iter().any(|intent| intent
            .content
            .iter()
            .all(|content| content.content.backend != Backend::Mprime)),
        "nothing ran on the other backend after the exhausted one was left"
    );
    assert!(ran_on(&events, Backend::Ycruncher) > 0);
}

#[test_case(Backend::Mprime; "mprime")]
#[test_case(Backend::Ycruncher; "ycruncher")]
fn a_backend_unavailable_from_the_start_is_exhausted_without_a_started_trial(backend: Backend) {
    let bench = Bench::new(&MachineDefinition::default(), 1);
    bench.sim.fail_backend_setup(backend);
    let finished = finish(bench.input());

    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    let Some(Stop::DeadEnd {
        dead_end, causes, ..
    }) = &finished.stop
    else {
        panic!("stop {:?}", finished.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::NoEvidence);
    assert!(
        dead_end
            .detail
            .starts_with("every runnable cell needs an unavailable backend:"),
        "{}",
        dead_end.detail
    );
    assert!(dead_end.detail.contains(&backend.to_string()));
    assert!(!causes.is_empty(), "the backend exhaustion has no evidence");
    let events = bench.events();
    let failed: Vec<_> = intents(&events)
        .into_iter()
        .filter(|intent| {
            intent
                .content
                .iter()
                .any(|content| content.content.backend == backend)
        })
        .collect();
    assert!(!failed.is_empty(), "the backend was never attempted");
    for intent in failed {
        assert!(
            find!(events, TrialStart, |start| start.trial == intent.trial).is_none(),
            "trial {} started on the failed backend",
            intent.trial
        );
        let end = find!(events, TrialEnd, |end| end.trial == intent.trial)
            .unwrap_or_else(|| panic!("the failed setup has no trial end"));
        assert_eq!(end.outcome, Outcome::Inconclusive);
    }
    let excluded = events
        .iter()
        .position(|event| {
            matches!(&event.payload, Payload::NightPlan(plan)
                if plan.unavailable.iter().any(|held| held.backend == backend))
        })
        .unwrap_or_else(|| panic!("the exhausted backend was never excluded from the night"));
    assert_eq!(ran_on(&events[excluded + 1..], backend), 0);
    for cause in causes {
        assert!(
            events.iter().any(|event| event.seq == *cause
                && matches!(&event.payload, Payload::TrialEnd(end)
                    if end.outcome == Outcome::Inconclusive)),
            "exhaustion cites a cause other than an inconclusive trial: {cause}"
        );
    }
    assert_no_zero_trial(&events);
    all_zero(&bench, 16);
}

#[test]
fn a_night_with_every_backend_exhausted_ends_in_a_dead_end() {
    let bench = Bench::new(&MachineDefinition::default(), 1);
    bench.sim.fail_backend_setup(Backend::Mprime);
    bench.sim.fail_backend_setup(Backend::Ycruncher);
    let finished = finish(bench.input());

    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    let Some(Stop::DeadEnd { dead_end, .. }) = finished.stop else {
        panic!("stop {:?}", finished.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::NoEvidence);
    assert!(
        dead_end
            .detail
            .starts_with("every runnable cell needs an unavailable backend:"),
        "{}",
        dead_end.detail
    );
    let events = bench.events();
    assert_eq!(count(&events, EventKind::TrialStart), 0);
    assert_eq!(count(&events, EventKind::Failure), 0);
    assert_no_zero_trial(&events);
    all_zero(&bench, 16);
}
