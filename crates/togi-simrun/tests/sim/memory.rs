use std::cell::Cell;
use std::rc::Rc;

use togi_journal::{Event, EventKind, Payload, ShutdownReason};
use togi_session::{SessionError, Stop};
use togi_simrun::Observe;

use crate::common::{Bench, finish, hunt_machine, state_json, until_kind};

fn count(events: &[Event], kind: EventKind) -> usize {
    events.iter().filter(|event| event.kind() == kind).count()
}

#[test]
fn a_failed_projection_warns_after_the_stop_and_does_not_fail_the_session() {
    let bench = Bench::new(&hunt_machine(2), 1);
    let mut input = bench.input();
    let fs = bench.fs.clone();
    input.observer = Some(Box::new(move |event| {
        if event.kind() == EventKind::Shutdown {
            fs.fail_state_publication();
        }
        Observe::Continue
    }));
    let finished = finish(input);
    assert_eq!(finished.stop, Some(Stop::Signal));
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);

    let events = bench.events();
    let [.., shutdown, warning] = events.as_slice() else {
        panic!("too few events");
    };
    assert!(
        matches!(&shutdown.payload, Payload::Shutdown(s) if s.reason == ShutdownReason::Signal)
    );
    let Payload::SessionWarning(warning) = &warning.payload else {
        panic!("last event is {}", events.last().unwrap().kind().as_str());
    };
    assert_eq!(warning.operation, "write state projection");
    assert!(!warning.error.is_empty());
    assert_eq!(finished.errors.len(), 0);
}

#[test]
fn a_crash_right_after_session_start_is_replayed_once() {
    let bench = Bench::new(&hunt_machine(2), 1);
    let mut input = bench.input();
    let sim = bench.sim.clone();
    let fired = Rc::new(Cell::new(false));
    let seen = Rc::clone(&fired);
    input.observer = Some(Box::new(move |event| {
        if event.kind() == EventKind::SessionStart && !seen.get() {
            seen.set(true);
            sim.crash();
            Observe::Continue
        } else if event.kind() == EventKind::TrialEnd {
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    let finished = finish(input);
    assert!(fired.get());
    assert_eq!(finished.stop, Some(Stop::Signal));

    let events = bench.events();
    assert_eq!(count(&events, EventKind::SessionStart), 1);
    assert_eq!(count(&events, EventKind::TrialEnd), 1);
    let crashes: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::CrashDetected(crash) = &event.payload {
                Some(crash)
            } else {
                None
            }
        })
        .collect();
    let [crash] = crashes.as_slice() else {
        panic!("{} crashes detected", crashes.len());
    };
    assert!(crash.stray);
    assert_eq!(crash.previous_boot, events[0].boot);
    assert_eq!(crash.in_flight, None);
    let last = events.last().unwrap();
    assert!(matches!(&last.payload, Payload::Shutdown(s) if s.reason == ShutdownReason::Signal));
    assert_eq!(bench.sim.violations(), Vec::<String>::new());

    let state: serde_json::Value = serde_json::from_slice(&state_json(&bench.fs).unwrap()).unwrap();
    assert_eq!(
        state["facts"]["last_seq"],
        serde_json::json!(last.seq.get())
    );
    assert_eq!(state["facts"]["cores"].as_array().unwrap().len(), 4);
}

#[test]
fn a_resume_reads_the_projection_the_earlier_run_published() {
    let definition = hunt_machine(2);
    let first = Bench::new(&definition, 1);
    let mut input = first.input();
    input.observer = Some(until_kind(EventKind::TrialEnd));
    assert_eq!(finish(input).stop, Some(Stop::Signal));
    let before = first.events().len();

    let second = first.resumed(&definition, 1);
    let mut input = second.input();
    input.observer = Some(until_kind(EventKind::TrialEnd));
    assert_eq!(finish(input).stop, Some(Stop::Signal));

    let events = second.events();
    assert_eq!(count(&events[before..], EventKind::StateRebuilt), 0);
    assert_eq!(count(&events[before..], EventKind::TrialEnd), 1);
}

#[test]
fn a_baseline_that_is_not_zero_starts_no_night() {
    let mut definition = hunt_machine(4);
    definition.bios = Some(vec![-5, 0, 0, 0]);
    let bench = Bench::new(&definition, 1);
    let finished = finish(bench.input());
    assert!(
        matches!(
            finished.errors.as_slice(),
            [SessionError::BaselineNotZero { .. }]
        ),
        "{:?}",
        finished.errors
    );
    let events = bench.events();
    assert_eq!(count(&events, EventKind::TrialIntent), 0);
    assert_eq!(count(&events, EventKind::ProfileApplied), 0);
}
