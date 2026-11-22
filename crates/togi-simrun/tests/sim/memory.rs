use std::cell::Cell;
use std::rc::Rc;

use togi_journal::{Event, EventKind, Payload, ShutdownReason};
use togi_machine::TargetOffset;
use togi_session::{SessionError, Stop};
use togi_sim::ScriptedTopology;
use togi_simrun::Observe;

use crate::common::{Bench, core, finish, passing, until_kind};

fn count(events: &[Event], kind: EventKind) -> usize {
    events.iter().filter(|event| event.kind() == kind).count()
}

#[test]
fn a_crash_right_after_session_start_is_replayed_once() {
    let bench = Bench::four_cores(1);
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

    finished.capsule.unwrap().verify(&events).unwrap();
}

#[test]
fn a_resume_replays_what_the_earlier_run_recorded() {
    let first = Bench::four_cores(1);
    let mut input = first.input();
    input.observer = Some(until_kind(EventKind::TrialEnd));
    assert_eq!(finish(input).stop, Some(Stop::Signal));
    let before = first.events().len();

    let second = first.from_history(1);
    let mut input = second.input();
    input.observer = Some(until_kind(EventKind::TrialEnd));
    assert_eq!(finish(input).stop, Some(Stop::Signal));

    let events = second.events();
    assert_eq!(count(&events[before..], EventKind::TrialEnd), 1);
}

#[test]
fn a_baseline_that_is_not_zero_starts_no_night() {
    let script = passing(ScriptedTopology::FourCores).with_boot_offsets(|id| {
        if id == core(0) {
            TargetOffset::new(-5).unwrap()
        } else {
            TargetOffset::ZERO
        }
    });
    let bench = Bench::scripted(script, 1);
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
