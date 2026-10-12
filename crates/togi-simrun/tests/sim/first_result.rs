use togi_journal::{CycleEvent, EventKind, Payload, ShutdownReason};
use togi_session::{Stop, StopPolicy};
use togi_tuner::PhaseNo;

use crate::common::{Bench, finish, machine_file, nz};

fn run(bench: &Bench, policy: StopPolicy) -> Option<Stop> {
    let mut input = bench.input();
    input.policy = policy;
    input.verify_every = None;
    let finished = finish(input);
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    finished.stop
}

const FIRST_RESULT: StopPolicy = StopPolicy {
    cycles: None,
    first_result: true,
};

#[test]
fn the_first_result_stops_after_phase_ones_passed_cycle() {
    let definition = machine_file("shared-voltage");
    let bench = Bench::new(&definition, 1000);
    assert_eq!(run(&bench, FIRST_RESULT), Some(Stop::FirstResult));

    let events = bench.events();
    assert!(
        events
            .iter()
            .all(|event| event.kind() != EventKind::DeepeningRound),
        "phase 2 ran a round"
    );
    assert!(events.iter().any(|event| matches!(
        &event.payload,
        Payload::CheckingCycle(cycle) if cycle.event == CycleEvent::Start
    )));
    let phase_one_end = events
        .iter()
        .position(|event| matches!(
            &event.payload,
            Payload::CheckingCycle(cycle) if cycle.event == CycleEvent::End && cycle.passed && cycle.full
        ))
        .expect("a passed full cycle");
    let last = events.last().unwrap();
    assert!(
        matches!(&last.payload, Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::FirstResult),
        "last event {} {}",
        last.kind().as_str(),
        last.msg
    );
    let after = &events[phase_one_end..];
    assert!(
        after
            .iter()
            .all(|event| event.kind() != EventKind::TrialIntent),
        "a trial started after phase 1's passed cycle"
    );
    assert!(
        after
            .iter()
            .any(|event| event.kind() == EventKind::ProfileRestored),
        "offsets were not restored before the shutdown"
    );

    let projection = togi_tuner::replay(&events).unwrap().project();
    let bios = projection.bios_profile.unwrap();
    assert_eq!(
        bios.confirmed.map(|confirmed| confirmed.seq),
        Some(events[phase_one_end].seq)
    );
    assert!(bios.unconfirmed.is_empty(), "{:?}", bios.unconfirmed);
    assert_eq!(
        projection.phase_plan.map(|plan| plan.phase),
        Some(PhaseNo::Two)
    );

    let resumed = bench.resumed(&definition, 1000);
    assert_eq!(run(&resumed, FIRST_RESULT), Some(Stop::FirstResult));
    let again = resumed.events();
    assert!(again.len() > events.len());
    let more_work = again[events.len()..].iter().find(|event| {
        matches!(
            event.kind(),
            EventKind::TrialIntent | EventKind::DeepeningRound | EventKind::CheckingCycle
        )
    });
    assert!(
        more_work.is_none(),
        "the resumed run did more work: {more_work:?}"
    );

    let cycles = StopPolicy {
        cycles: Some(nz(1)),
        first_result: false,
    };
    assert_eq!(
        run(&resumed.resumed(&definition, 1000), cycles),
        Some(Stop::Cycles)
    );
}
