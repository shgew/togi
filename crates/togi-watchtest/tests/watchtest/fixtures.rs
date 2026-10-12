use togi_journal::{Activity, CycleEvent, Decision, EventKind, Outcome, Payload};
use togi_watchtest::{Base, cycle_completed_by_rerun};

use crate::support::{fold, intent_of};

#[test]
fn reruns_that_pass_complete_the_cycle_before_the_next_cycle_starts() {
    let journals = cycle_completed_by_rerun().expect("the fixture builds");
    assert!(!journals.running.is_empty());
    for events in &journals.running {
        let Some(Payload::TrialStart(start)) = events.last().map(|event| &event.payload) else {
            panic!("a running journal ends on trial.start");
        };
        assert!(intent_of(events, &start.trial).rerun, "a rerun runs");
        assert!(!events.iter().any(
            |event| matches!(&event.payload, Payload::TrialEnd(end) if end.trial == start.trial)
        ));
    }
    let Some(Payload::TrialEnd(end)) = journals.passed.last().map(|event| &event.payload) else {
        panic!("the passed journal ends on trial.end");
    };
    assert_eq!(end.outcome, Outcome::Pass);
    assert!(intent_of(&journals.passed, &end.trial).rerun);
    let Some(Payload::CheckingCycle(cycle)) = journals.ended.last().map(|event| &event.payload)
    else {
        panic!("the ended journal ends on the cycle's end");
    };
    assert_eq!((cycle.cycle.get(), cycle.event), (1, CycleEvent::End));
    assert!(journals.ended.starts_with(&journals.passed));
    assert!(
        journals
            .ended
            .iter()
            .filter_map(|event| match &event.payload {
                Payload::TrialIntent(intent) => Some(intent),
                _ => None,
            })
            .all(|intent| intent.cycle.is_none_or(|cycle| cycle.get() == 1)),
        "no cycle 2 trial has begun"
    );
    let r6_failed = journals.ended.iter().any(|event| {
        matches!(&event.payload, Payload::TrialEnd(end) if end.outcome == Outcome::Failure && end.core.is_some_and(|core| core.get() == 0))
    });
    assert!(r6_failed, "R6 failed naming core 00");
    fold(&journals.ended);
}

#[test]
fn the_crashed_cycle_session_backs_off_after_a_crashed_checking_trial() {
    let events = Base::CrashedCycle.events().expect("the base simulates");
    let crashed_checking: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::CrashDetected(crash) => crash.in_flight.map(|seq| (event.seq, seq)),
            _ => None,
        })
        .filter(|(_, in_flight)| {
            let at = usize::try_from(in_flight.get() - 1).expect("seq fits");
            matches!(&events[at].payload, Payload::TrialIntent(intent) if intent.activity == Some(Activity::Checking) && intent.cycle.is_some())
        })
        .collect();
    assert!(
        !crashed_checking.is_empty(),
        "a checking cycle trial crashed"
    );
    let moved_on = crashed_checking.iter().any(|(detected, _)| {
        events.iter().any(|event| {
            event.seq > *detected
                && matches!(&event.payload, Payload::TunerDecision(decision) if decision.decision == Decision::Backoff)
        })
    });
    assert!(moved_on, "a backoff followed the crash");
    assert!(
        events
            .iter()
            .any(|event| event.kind() == EventKind::Shutdown)
    );
}
