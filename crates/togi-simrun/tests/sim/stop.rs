use jiff::SignedDuration;
use togi_journal::{EventKind, Payload, ShutdownReason};
use togi_session::Stop;

use crate::common::{Bench, finish, hunt_machine, until_kind};

#[test]
fn an_observer_stops_the_run_after_the_first_matching_append() {
    let bench = Bench::new(&hunt_machine(4), 1);
    let mut input = bench.input();
    input.observer = Some(until_kind(EventKind::TrialEnd));
    let finished = finish(input);
    assert_eq!(finished.stop, Some(Stop::Signal));
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    let ends = bench
        .events()
        .iter()
        .filter(|event| matches!(event.payload, Payload::TrialEnd(_)))
        .count();
    assert_eq!(ends, 1);
    let last = bench.events().pop().unwrap();
    assert!(
        matches!(&last.payload, Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal)
    );
}

#[test]
fn the_operator_ends_the_night_at_the_first_event_at_or_after_stop_at() {
    let bench = Bench::new(&hunt_machine(4), 1);
    let stop_at = bench.sim.now() + SignedDuration::from_mins(30);
    let mut input = bench.input();
    input.stop_at = Some(stop_at);
    let finished = finish(input);
    assert_eq!(finished.stop, Some(Stop::Signal));
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);

    let events = bench.events();
    let (shutdown, before) = events.split_last().unwrap();
    assert!(
        matches!(&shutdown.payload, Payload::Shutdown(s) if s.reason == ShutdownReason::Signal)
    );
    let late: Vec<_> = before
        .iter()
        .filter(|event| event.time >= stop_at)
        .collect();
    assert!(!late.is_empty(), "the night ended before stop_at");
    assert!(
        late.iter()
            .all(|event| !matches!(event.payload, Payload::TrialIntent(_))),
        "a trial started after the operator ended the night"
    );
}

#[test]
fn a_stop_time_already_past_ends_the_night_at_its_first_event() {
    let bench = Bench::new(&hunt_machine(4), 1);
    let mut input = bench.input();
    input.stop_at = Some(bench.sim.now());
    let finished = finish(input);
    assert_eq!(finished.stop, Some(Stop::Signal));
    let events = bench.events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, Payload::TrialEnd(_))),
        "a trial ran after the operator had ended the night"
    );
}
