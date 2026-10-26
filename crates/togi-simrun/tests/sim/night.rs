//! Whole Ruleset 13 nights through the simulation: the real session over the simulated seams, ended by the simulated
//! operator at `stop_at` or by a dead end.

use jiff::SignedDuration;
use test_case::test_case;
use togi_journal::{
    DeadEndCondition, Event, EventKind, Payload, Queue, ShutdownReason, TrialIntent,
};
use togi_machine::{Backend, CoreId, Signal};
use togi_session::{SessionError, Stop};
use togi_sim::MachineDefinition;
use togi_sim::definition::ScriptDef;
use togi_simrun::{Observe, Replay};
use togi_tuner::policy::EndReason;

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

#[test]
fn a_first_night_runs_its_controls_then_maps_until_the_operator_ends_it() {
    let bench = Bench::new(&MachineDefinition::default(), 1);
    let finished = finish(bench.input());

    assert_eq!(finished.stop, Some(Stop::Signal));
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    let events = bench.events();
    let queues: Vec<Queue> = intents(&events)
        .iter()
        .filter_map(|intent| intent.task.map(|task| task.queue))
        .collect();
    assert_eq!(queues.first(), Some(&Queue::Control), "{queues:?}");
    let first_mapping = queues
        .iter()
        .position(|queue| *queue == Queue::Mapping)
        .expect("the night maps");
    assert!(
        queues[first_mapping..]
            .iter()
            .all(|queue| *queue != Queue::Control),
        "{queues:?}"
    );
    assert_eq!(shutdown_reasons(&events), [ShutdownReason::Signal]);
    let nights: Vec<_> = intents(&events).iter().map(|intent| intent.night).collect();
    assert!(
        nights.windows(2).all(|pair| pair[0] == pair[1]),
        "{nights:?}"
    );
    all_zero(&bench, 8);
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
fn a_stock_unstable_machine_dead_ends_at_zero_before_the_operator_ends_the_night() {
    let mut definition = hunt_machine(4);
    definition.script = vec![ScriptDef {
        trial: "0001".into(),
        signal: Some(Signal::Crash),
        ..ScriptDef::default()
    }];
    let bench = Bench::new(&definition, 1);
    let finished = finish(bench.input());

    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    match &finished.stop {
        Some(Stop::DeadEnd { dead_end, .. }) => {
            assert_eq!(dead_end.condition, DeadEndCondition::FailureAtZero);
        }
        Some(Stop::Night(EndReason::DeadEnd)) => {}
        other => panic!("the night did not dead end: {other:?}"),
    }
    let events = bench.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(&event.payload, Payload::Failure(failure) if failure.signal == Signal::Crash)),
        "the dead end has no failure"
    );
    assert_eq!(
        shutdown_reasons(&events).last(),
        Some(&ShutdownReason::DeadEnd)
    );
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
    let mut input = bench.input();
    // The first night's stock controls need both backends, so the backend fails after the first mapping trial.
    input.observer = Some(Box::new(move |event| {
        if let Payload::TrialIntent(intent) = &event.payload
            && intent.task.is_some_and(|task| task.queue == Queue::Mapping)
        {
            mapping = true;
        } else if matches!(event.payload, Payload::TrialEnd(_)) && mapping && !failed {
            failed = true;
            sim.fail_backend_setup(Backend::Mprime);
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
fn first_night_controls_that_need_an_exhausted_backend_end_the_night_without_evidence(
    backend: Backend,
) {
    let bench = Bench::new(&MachineDefinition::default(), 1);
    bench.sim.fail_backend_setup(backend);
    let finished = finish(bench.input());

    assert!(
        finished
            .errors
            .iter()
            .all(|error: &SessionError| !error.is_crash()),
        "{:?}",
        finished.errors
    );
    let Some(Stop::DeadEnd { dead_end, .. }) = &finished.stop else {
        panic!("the night did not dead end: {:?}", finished.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::NoEvidence);
}

#[test]
fn a_night_with_every_backend_exhausted_ends_in_a_dead_end() {
    let bench = Bench::new(&MachineDefinition::default(), 1);
    bench.sim.fail_backend_setup(Backend::Mprime);
    bench.sim.fail_backend_setup(Backend::Ycruncher);
    let finished = finish(bench.input());

    assert!(
        finished
            .errors
            .iter()
            .all(|error: &SessionError| !error.is_crash()),
        "{:?}",
        finished.errors
    );
    assert!(
        matches!(
            finished.stop,
            Some(Stop::DeadEnd { .. } | Stop::Night(EndReason::Unavailable))
        ),
        "{:?}",
        finished.stop
    );
}
