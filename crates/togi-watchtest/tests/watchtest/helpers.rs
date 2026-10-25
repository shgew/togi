use togi_journal::EventKind;
use togi_journal::{Activity, Outcome, Payload};
use togi_machine::CoreId;
use togi_sim::MachineDefinition;
use togi_watchtest::{
    Error, Scenario, before_crash_detected, crash_trial, cut_at, cut_passed_trial, cut_trial,
    cut_when,
};

use crate::support::intent_of;

#[test]
fn cut_at_ends_on_the_first_event_it_accepts() {
    let events = Scenario::Between.events().expect("the scenario builds");
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
    let events = Scenario::Controls.events().expect("the scenario builds");
    assert!(matches!(cut_at(&events, |_| false), Err(Error::NoCut(_))));
    assert!(matches!(cut_at(&[], |_| true), Err(Error::NoCut(_))));
    assert!(matches!(
        cut_trial(&events, |_| false),
        Err(Error::NoCut(_))
    ));
    assert!(matches!(
        cut_passed_trial(&events, |_| true),
        Err(Error::NoCut(_))
    ));
    assert!(matches!(
        cut_when(&events, |_, _| false),
        Err(Error::NoCut(_))
    ));
}

#[test]
fn cut_when_hands_over_the_policy_state_after_the_event() {
    let events = Scenario::Controls.events().expect("the scenario builds");
    let mut seen = Vec::new();
    let cut = cut_when(&events, |state, event| {
        seen.push(event.seq);
        state.project().night.is_some()
    })
    .expect("the session plans its night");
    assert!(
        matches!(
            cut.last().map(|event| &event.payload),
            Some(Payload::NightPlan(_))
        ),
        "the cut ends on the event whose fold made the plan"
    );
    assert_eq!(seen, cut.iter().map(|event| event.seq).collect::<Vec<_>>());
}

#[test]
fn cut_trial_ends_on_the_start_of_the_first_trial_it_accepts() {
    let events = Scenario::Between.events().expect("the scenario builds");
    let cut = cut_trial(&events, |_| true).expect("a trial started");
    let Some(Payload::TrialStart(start)) = cut.last().map(|event| &event.payload) else {
        panic!("a trial cut ends on trial.start");
    };
    assert_eq!(start.trial.as_str(), "0001");
}

#[test]
fn before_crash_detected_needs_a_crash() {
    let events = Scenario::Controls.events().expect("the scenario builds");
    assert!(matches!(
        before_crash_detected(&events),
        Err(Error::NoCut(_))
    ));
}

#[test]
fn cut_passed_trial_ends_on_the_first_accepted_trial_that_passed() {
    let events = Scenario::Between.events().expect("the scenario builds");
    let cut = cut_passed_trial(&events, |intent| intent.activity == Some(Activity::Control))
        .expect("a control trial passed");
    let Some(Payload::TrialEnd(end)) = cut.last().map(|event| &event.payload) else {
        panic!("the cut ends on trial.end");
    };
    assert_eq!(end.outcome, Outcome::Pass);
    assert_eq!(intent_of(cut, &end.trial).activity, Some(Activity::Control));
    let earlier_pass = cut[..cut.len() - 1]
        .iter()
        .any(|event| matches!(&event.payload, Payload::TrialEnd(earlier) if earlier.outcome == Outcome::Pass));
    assert!(!earlier_pass, "no trial passed before it");
}

#[test]
fn a_scenario_that_cannot_be_cut_names_itself_in_the_error() {
    let error = Scenario::Controls
        .events_from(&[])
        .expect_err("an empty journal has no control trial");
    assert!(
        matches!(&error, Error::Scenario { scenario: Scenario::Controls, source } if matches!(**source, Error::NoCut(_)))
    );
    assert_eq!(error.to_string(), "scenario controls");
    assert_eq!(
        togi_machine::error_chain(&error),
        "scenario controls: no cut point in the simulated journal: no event the cut accepts"
    );
}

#[test]
fn a_crash_scripted_on_a_core_the_machine_lacks_is_an_error() {
    let two = MachineDefinition {
        cores: 2,
        ..MachineDefinition::default()
    };
    let machine = togi_watchtest::machine(&two, 1).expect("the machine builds");
    let three = CoreId::new(3).expect("core 03 exists");
    assert!(matches!(
        crash_trial(&machine, "0001", 7.0, three),
        Err(Error::Script(_))
    ));
}
