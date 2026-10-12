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
