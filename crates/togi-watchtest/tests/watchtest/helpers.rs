use togi_journal::{EventKind, Payload};
use togi_tuner::PhaseNo;
use togi_watchtest::{Error, Scenario, before_crash_detected, cut_at, cut_trial, cut_when};

#[test]
fn cut_at_ends_on_the_first_event_it_accepts() {
    let events = Scenario::Search.events().expect("the scenario builds");
    let cut = cut_at(&events, |event| event.kind() == EventKind::TrialEnd).expect("a trial ended");
    assert_eq!(
        cut.last().map(togi_journal::Event::kind),
        Some(EventKind::TrialEnd)
    );
    assert_eq!(
        cut.iter()
            .filter(|event| event.kind() == EventKind::TrialEnd)
            .count(),
        1
    );
    assert!(events.starts_with(cut));
}

#[test]
fn a_cut_nothing_accepts_is_an_error() {
    let events = Scenario::Search.events().expect("the scenario builds");
    assert!(matches!(cut_at(&events, |_| false), Err(Error::NoCut(_))));
    assert!(matches!(cut_at(&[], |_| true), Err(Error::NoCut(_))));
    assert!(matches!(
        cut_trial(&events, |_| false),
        Err(Error::NoCut(_))
    ));
}

#[test]
fn cut_when_hands_over_the_tuner_state_after_the_event() {
    let events = Scenario::Search.events().expect("the scenario builds");
    let mut seen = Vec::new();
    let cut = cut_when(&events, |state, event| {
        seen.push(event.seq);
        state
            .phase_plan()
            .is_some_and(|plan| plan.phase == PhaseNo::One)
            && event.kind() == EventKind::TrialEnd
    })
    .expect("a trial ended in phase 1");
    assert_eq!(
        seen.len(),
        cut.len(),
        "the accept sees every event in order"
    );
}

#[test]
fn cut_trial_ends_on_the_start_of_the_first_trial_it_accepts() {
    let events = Scenario::Search.events().expect("the scenario builds");
    let cut = cut_trial(&events, |_| true).expect("a trial started");
    let Some(Payload::TrialStart(start)) = cut.last().map(|event| &event.payload) else {
        panic!("a trial cut ends on trial.start");
    };
    assert_eq!(start.trial.as_str(), "0001");
}

#[test]
fn before_crash_detected_needs_a_crash() {
    let events = Scenario::Search.events().expect("the scenario builds");
    assert!(matches!(
        before_crash_detected(&events),
        Err(Error::NoCut(_))
    ));
}
