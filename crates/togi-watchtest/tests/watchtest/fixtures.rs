use togi_journal::{Activity, EventKind, Outcome, Payload, Queue, ShutdownReason};
use togi_watchtest::Base;

#[test]
fn the_night_base_passes_every_control_until_the_operator_ends_it() {
    let events = Base::Night.events().expect("the base simulates");
    assert!(matches!(
        &events.last().expect("events").payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal
    ));
    for event in events {
        if let Payload::TrialIntent(intent) = &event.payload {
            assert_eq!(
                intent.activity,
                Some(Activity::Control),
                "#{}",
                event.seq.get()
            );
        }
        if let Payload::TrialEnd(end) = &event.payload {
            assert_ne!(end.outcome, Outcome::Failure, "#{}", event.seq.get());
        }
    }
}

#[test]
fn the_dead_end_base_crashes_its_fourth_control_trial() {
    let events = Base::DeadEnd.events().expect("the base simulates");
    let crash = events
        .iter()
        .position(|event| event.kind() == EventKind::CrashDetected)
        .expect("a crash is recorded");
    let passed = events[..crash]
        .iter()
        .filter(|event| matches!(&event.payload, Payload::TrialEnd(_)))
        .count();
    assert_eq!(passed, 3, "three control trials passed before the crash");
    assert!(matches!(
        &events.last().expect("events").payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::DeadEnd
    ));
}

#[test]
fn the_mapped_base_stops_at_a_natural_crash_of_a_mapping_trial() {
    let events = Base::Mapped.events().expect("the base simulates");
    let crash = events
        .iter()
        .position(|event| event.kind() == EventKind::CrashDetected)
        .expect("a crash is recorded");
    let Payload::CrashDetected(detected) = &events[crash].payload else {
        unreachable!("the position is a crash.detected");
    };
    let flight = detected.in_flight.expect("a trial was in flight");
    let Payload::TrialIntent(intent) =
        &events[usize::try_from(flight.get() - 1).expect("an index")].payload
    else {
        panic!("the in-flight seq names a trial intent");
    };
    assert_eq!(intent.task.map(|task| task.queue), Some(Queue::Mapping));
    assert!(
        events[crash + 1..]
            .iter()
            .all(|event| event.kind() != EventKind::TrialIntent),
        "the base halts once the crash is recorded"
    );
}

#[test]
fn the_filler_base_halts_after_its_first_filler_trial() {
    let events = Base::Filler.events().expect("the base simulates");
    let fillers: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload
                && intent.task.is_some_and(|task| task.queue == Queue::Filler)
            {
                Some(intent.trial.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(fillers.len(), 1, "{fillers:?}");
    assert!(
        events.iter().any(
            |event| matches!(&event.payload, Payload::TrialEnd(end) if end.trial == fillers[0])
        )
    );
}

/// Regenerate fixtures explicitly, outside the ordinary in-memory tests:
/// `TOGI_FIXTURES="${TMPDIR:-/tmp}/togi-watchtest-fixtures" cargo test -p togi-watchtest real_fs_generate_fixtures -- --ignored`.
/// Review and copy the emitted files into `src/fixtures/`.
#[test]
#[ignore = "writes regenerated journal fixtures to an explicitly supplied temporary directory"]
fn real_fs_generate_fixtures() {
    use std::fmt::Write as _;
    use std::io::Write as _;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use togi_watchtest::Scenario;

    let directory = std::env::var_os("TOGI_FIXTURES").expect("set TOGI_FIXTURES");
    let directory = std::path::Path::new(&directory);
    assert!(
        directory.starts_with(std::env::temp_dir()),
        "use a temporary directory"
    );
    std::fs::create_dir_all(directory).expect("create the fixture output directory");
    let mut cuts = String::from("match self {\n");
    for base in Base::ALL {
        let events = base.simulate().expect("the base simulates");
        let mut journal = GzEncoder::new(Vec::new(), Compression::default());
        for event in &events {
            writeln!(journal, "{}", event.encode().expect("an event encodes"))
                .expect("compress a fixture event");
        }
        std::fs::write(
            directory.join(format!("{base:?}.jsonl.gz")),
            journal.finish().expect("finish the compressed fixture"),
        )
        .expect("write the base fixture");
        for scenario in Scenario::ALL
            .into_iter()
            .filter(|scenario| scenario.base() == base)
        {
            let events = scenario
                .events_from(&events)
                .expect("the scenario builds from the fresh simulation");
            writeln!(cuts, "    Self::{scenario:?} => {},", events.len())
                .expect("write the scenario cut");
        }
    }
    cuts.push_str("}\n");
    std::fs::write(directory.join("scenario-cuts.inc"), cuts)
        .expect("write the scenario cut fixtures");
}
