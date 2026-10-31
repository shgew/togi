use togi_journal::{
    Activity, DeadEndCondition, EventKind, Outcome, Payload, Queue, ShutdownReason,
};
use togi_watchtest::Base;

use crate::support::intent_of;

#[test]
fn the_night_base_runs_only_nonzero_work_until_the_operator_ends_it() {
    let events = Base::Night.events().expect("the base simulates");
    assert!(matches!(
        &events.last().expect("events").payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal
    ));
    let mut failures = 0_usize;
    for event in events {
        if let Payload::TrialIntent(intent) = &event.payload {
            assert!(
                matches!(
                    intent.activity,
                    Some(Activity::Mapping | Activity::Qualification)
                ),
                "#{}",
                event.seq.get()
            );
            assert!(
                intent.profile.iter().any(|(_, offset)| offset.get() != 0),
                "#{}",
                event.seq.get()
            );
        }
        if let Payload::TrialEnd(end) = &event.payload
            && end.outcome == Outcome::Failure
        {
            failures += 1;
            let failed = intent_of(events, &end.trial);
            assert!(
                failed.profile.iter().any(|(_, offset)| offset.get() != 0),
                "#{}: a failure at stock",
                event.seq.get()
            );
        }
        if let Payload::DeadEnd(dead) = &event.payload {
            assert_ne!(
                dead.condition,
                DeadEndCondition::FailureAtZero,
                "#{}",
                event.seq.get()
            );
        }
    }
    assert!(
        failures > 0,
        "the default machine fails nonzero mapping trials in its first night"
    );
}

#[test]
fn the_dead_end_base_exhausts_nonzero_probes_without_testing_stock() {
    let events = Base::DeadEnd.events().expect("the base simulates");
    assert!(
        events
            .iter()
            .any(|event| event.kind() == EventKind::CrashDetected)
    );
    assert!(events.iter().all(|event| !matches!(&event.payload,
        Payload::TrialIntent(intent) if intent.profile.iter().all(|(_, offset)| offset.get() == 0)
            || intent.activity == Some(Activity::Control))));
    let dead = events
        .iter()
        .find_map(|event| {
            if let Payload::DeadEnd(dead) = &event.payload {
                Some(dead)
            } else {
                None
            }
        })
        .expect("exhausted mapping ends with named no evidence");
    assert_eq!(dead.condition, DeadEndCondition::NoEvidence);
    assert!(dead.detail.starts_with("no_nonzero_work:"));
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
fn the_filler_base_halts_at_its_first_nonzero_filler_trial_start() {
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
    assert!(matches!(&events.last().expect("events").payload,
        Payload::TrialStart(start) if start.trial == fillers[0]));
    assert!(events.iter().any(|event| matches!(&event.payload,
        Payload::TrialIntent(intent) if intent.trial == fillers[0] && intent.profile.iter().any(|(_, offset)| offset.get() != 0))));
}
