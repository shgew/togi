use std::collections::BTreeSet;

use jiff::SignedDuration;
use togi_journal::{
    Activity, DeadEndCondition, Event, EventKind, Outcome, Payload, QualificationState, Queue,
    ShutdownReason, StepKind, TaskStatus, Tested,
};
use togi_machine::{Regime, TargetOffset};
use togi_watchtest::{Base, Scenario};

use crate::support::{intent_of, project, running_trial};

fn cut(scenario: Scenario) -> Vec<Event> {
    scenario.events().expect("the scenario builds")
}

#[test]
fn every_scenario_names_a_distinct_golden_and_builds() {
    let names: Vec<&str> = Scenario::ALL
        .iter()
        .map(|scenario| scenario.name())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "{names:?}");
    for want in [
        "controls",
        "idle",
        "between",
        "mapping",
        "crashed",
        "recovering",
        "filler",
        "qualification",
        "day-step",
        "reproduction",
        "deadend",
        "stopped",
    ] {
        assert!(names.contains(&want), "{want} is missing from {names:?}");
    }
    for scenario in Scenario::ALL {
        assert!(!cut(scenario).is_empty(), "{scenario}");
    }
}

#[test]
fn every_scenario_is_a_prefix_of_its_base_simulation() {
    for scenario in Scenario::ALL {
        let base = scenario.base().events().expect("the base simulates");
        assert!(base.starts_with(&cut(scenario)), "{scenario}");
    }
}

#[test]
fn every_session_scenario_but_the_ended_night_folds_into_one_open_night() {
    for scenario in Scenario::ALL
        .into_iter()
        .filter(|scenario| scenario.base() != Base::Calendar && *scenario != Scenario::Stopped)
    {
        let events = cut(scenario);
        let projection = project(&events);
        let night = projection.night.expect("the night is planned");
        assert_eq!(
            night.closed_by, None,
            "{scenario}: no day boot closed the night"
        );
        assert_eq!(
            night.started, events[0].time,
            "{scenario}: the night opened as the session started"
        );
        assert!(!projection.ccds.is_empty(), "{scenario}");
    }
}

#[test]
fn controls_starts_the_first_nights_zero_offset_control_trial() {
    let events = cut(Scenario::Controls);
    let intent = running_trial(&events);
    assert_eq!(intent.activity, Some(Activity::Control));
    assert_eq!(intent.tested, Some(Tested::Exact {}));
    assert!(intent.profile.iter().all(|(_, offset)| offset.get() == 0));
    assert_eq!(intent.task.map(|task| task.queue), Some(Queue::Control));
    assert!(!intent.retry);
    assert_eq!(intent.regime, Regime::R1);
    let night = project(&events).night.expect("the night is planned");
    let (task, activity) = night.current.expect("the control task runs");
    assert_eq!(task.queue, Queue::Control);
    assert_eq!(activity, Some(Activity::Control));
    assert!(night.mapping.iter().all(|mapping| mapping.cursor.is_some()));
    assert!(
        project(&events).ccds.iter().all(|ccd| ccd.cap.is_none()),
        "no cap is committed before any exposure"
    );
}

#[test]
fn idle_starts_the_first_r6_trial_on_every_core() {
    let events = cut(Scenario::Idle);
    let intent = running_trial(&events);
    assert_eq!(intent.regime, Regime::R6);
    assert_eq!(intent.activity, Some(Activity::Control));
    assert_eq!(intent.cores.len(), 16);
    let earlier = events.iter().filter(
        |event| matches!(&event.payload, Payload::TrialIntent(other) if other.regime == Regime::R6),
    );
    assert_eq!(earlier.count(), 1, "no R6 trial came before");
}

#[test]
fn between_ends_on_a_passed_trial_with_none_in_flight() {
    let events = cut(Scenario::Between);
    let Payload::TrialEnd(end) = &events.last().expect("events").payload else {
        panic!("the journal ends on {:?}", events.last().map(Event::kind));
    };
    assert_eq!(end.outcome, Outcome::Pass);
    let passed = intent_of(&events, &end.trial);
    assert_eq!(passed.activity, Some(Activity::Control));
    assert_eq!(passed.regime, Regime::R3);
    assert!(
        togi_journal::State::replay(&events)
            .expect("the journal folds")
            .in_flight()
            .is_none(),
        "no trial is open between trials"
    );
    assert!(
        project(&events)
            .night
            .expect("planned")
            .current
            .is_some_and(|(task, _)| task.queue == Queue::Control),
        "the control task stays open between its trials"
    );
}

#[test]
fn mapping_starts_the_first_isolated_trial_below_zero_after_the_controls() {
    let events = cut(Scenario::Mapping);
    let intent = running_trial(&events);
    assert_eq!(intent.activity, Some(Activity::Mapping));
    assert_eq!(intent.task.map(|task| task.queue), Some(Queue::Mapping));
    let Some(Tested::Isolated { offset, .. }) = intent.tested else {
        panic!("a mapping trial tests one CCD, not {:?}", intent.tested);
    };
    assert_eq!(
        offset,
        TargetOffset::new(-5).expect("an offset"),
        "mapping descends by five counts"
    );
    let offsets: BTreeSet<TargetOffset> = intent.profile.iter().map(|(_, offset)| offset).collect();
    assert_eq!(
        offsets,
        BTreeSet::from([offset, TargetOffset::ZERO]),
        "the tested CCD runs the offset, every other CCD stock"
    );
    let night = project(&events).night.expect("planned");
    assert!(
        night
            .tasks
            .iter()
            .filter(|planned| planned.task.queue == Queue::Control)
            .all(|planned| planned.status == TaskStatus::Complete),
        "the controls finished before mapping went below zero: {:?}",
        night.tasks
    );
    assert_eq!(
        night.current.map(|(task, activity)| (task.queue, activity)),
        Some((Queue::Mapping, Some(Activity::Mapping)))
    );
}

#[test]
fn filler_starts_once_mapping_has_nothing_left() {
    let events = cut(Scenario::Filler);
    let intent = running_trial(&events);
    assert_eq!(intent.task.map(|task| task.queue), Some(Queue::Filler));
    assert!(
        matches!(intent.tested, Some(Tested::Isolated { offset, .. }) if offset != TargetOffset::ZERO),
        "filler tests one CCD past its cap: {:?}",
        intent.tested
    );
    let night = project(&events).night.expect("planned");
    assert!(
        night
            .tasks
            .iter()
            .all(|planned| planned.task.queue == Queue::Filler
                || planned.status != TaskStatus::Pending),
        "no other task is due: {:?}",
        night.tasks
    );
    assert!(
        !events.iter().any(|event| matches!(&event.payload,
            Payload::TrialIntent(other) if other.task.is_some_and(|task| task.queue == Queue::Filler)
                && other.trial != intent.trial)),
        "it is the first filler trial"
    );
}

#[test]
fn qualification_starts_an_exact_trial_of_a_profile_below_zero() {
    let events = cut(Scenario::Qualification);
    let intent = running_trial(&events);
    assert_eq!(intent.activity, Some(Activity::Qualification));
    assert_eq!(intent.tested, Some(Tested::Exact {}));
    assert_eq!(
        intent.task.map(|task| task.queue),
        Some(Queue::Qualification)
    );
    assert!(
        intent
            .profile
            .iter()
            .any(|(_, offset)| offset != TargetOffset::ZERO),
        "a qualification tests a profile below stock"
    );
    let night = project(&events).night.expect("planned");
    assert_eq!(night.closed_by, None, "the night is open");
    assert!(
        night.qualification.is_some(),
        "the night shows the qualification's progress"
    );
}

#[test]
fn day_step_ends_on_the_first_promotion_after_a_complete_qualification() {
    let events = cut(Scenario::DayStep);
    let Payload::DayStep(step) = &events.last().expect("events").payload else {
        panic!("the journal ends on {:?}", events.last().map(Event::kind));
    };
    assert!(
        matches!(step.step, StepKind::Promote { .. }),
        "{:?}",
        step.step
    );
    assert_ne!(step.proposed, step.old);
    assert!(
        events.iter().any(|event| matches!(&event.payload,
            Payload::NightQualification(qualification)
                if matches!(qualification.state, QualificationState::Complete { .. }))),
        "a night qualified the profile first"
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind() == EventKind::DayBootStart && event.boot == step.cold_boot),
        "the step is decided in a day boot the observer recorded"
    );
}

#[test]
fn reproduction_retests_the_cpu_suspect_profile_exactly() {
    let events = cut(Scenario::Reproduction);
    let intent = running_trial(&events);
    assert_eq!(intent.activity, Some(Activity::Reproduction));
    assert_eq!(
        intent.task.map(|task| task.queue),
        Some(Queue::Reproduction)
    );
    let suspect = events
        .iter()
        .find_map(|event| {
            if let Payload::DayUncleanEnd(end) = &event.payload {
                Some(end.offsets)
            } else {
                None
            }
        })
        .expect("a day boot ended uncleanly");
    assert!(
        intent.profile.iter().all(|(core, offset)| suspect
            .get(core)
            .is_some_and(|observed| observed.0 == offset.get())),
        "the reproduction runs the profile the crashed day ran"
    );
    let night = project(&events).night.expect("planned");
    assert_eq!(night.reproductions.len(), 1, "{:?}", night.reproductions);
    let reproductions = events
        .iter()
        .filter(|event| {
            matches!(&event.payload,
                Payload::TrialIntent(other) if other.activity == Some(Activity::Reproduction))
        })
        .count();
    assert_eq!(reproductions, 1, "it is the first reproduction trial");
}

/// The intent of the trial a crashed journal leaves in flight: a mapping trial below zero.
fn crashed_mapping_trial(events: &[Event]) {
    let open = togi_journal::State::replay(events)
        .expect("the journal folds")
        .in_flight()
        .and_then(|flight| flight.trial.clone())
        .expect("the crashed trial is still in flight");
    let intent = intent_of(events, &open);
    assert_eq!(intent.task.map(|task| task.queue), Some(Queue::Mapping));
    assert!(
        matches!(intent.tested, Some(Tested::Isolated { offset, .. }) if offset != TargetOffset::ZERO),
        "{:?}",
        intent.tested
    );
}

#[test]
fn crashed_ends_before_the_next_boot_records_the_crash() {
    let events = cut(Scenario::Crashed);
    assert!(
        events
            .iter()
            .all(|event| event.kind() != EventKind::CrashDetected)
    );
    let last = events.last().expect("events");
    let first = events.first().expect("events");
    assert_ne!(last.boot, first.boot, "the journal ends in the next boot");
    crashed_mapping_trial(&events);
}

#[test]
fn recovering_ends_on_the_recorded_crash() {
    let events = cut(Scenario::Recovering);
    let last = events.last().expect("events");
    assert!(matches!(last.payload, Payload::CrashDetected(_)));
    assert_eq!(
        events.len(),
        cut(Scenario::Crashed).len() + 1,
        "recovering is crashed plus the crash record"
    );
    let night = project(&events).night.expect("planned");
    assert_eq!(
        night.night,
        project(&events[..1_000]).night.expect("planned").night,
        "the crash reboot continues the same night"
    );
}

#[test]
fn deadend_ends_on_a_failure_at_stock() {
    let events = cut(Scenario::Deadend);
    let Payload::DeadEnd(dead) = &events.last().expect("events").payload else {
        panic!("the journal ends on {:?}", events.last().map(Event::kind));
    };
    assert_eq!(dead.condition, DeadEndCondition::FailureAtZero);
    assert!(
        events
            .iter()
            .any(|event| event.kind() == EventKind::CrashDetected),
        "the crash at CO 0 is recorded before the dead end"
    );
    assert!(project(&events).stopped);
}

#[test]
fn stopped_is_a_night_the_operator_ended_with_work_left() {
    let events = cut(Scenario::Stopped);
    assert!(matches!(
        &events.last().expect("events").payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal
    ));
    let first = events.first().expect("events").time;
    let last = events.last().expect("events").time;
    assert!(
        last.duration_since(first) >= SignedDuration::from_hours(8),
        "the operator ended the night after eight hours: {first} to {last}"
    );
    let projection = project(&events);
    assert!(!projection.stopped, "an operator end is not a dead end");
    let night = projection.night.expect("planned");
    assert_eq!(
        night.closed_by,
        events.last().map(|event| event.seq),
        "the operator's signal closes the night"
    );
    assert!(
        night
            .tasks
            .iter()
            .any(|planned| planned.status != TaskStatus::Complete),
        "work is left: {:?}",
        night.tasks
    );
}
#[test]
fn the_default_night_does_not_end_before_it_has_run_its_controls() {
    let base = Base::Night.events().expect("the base simulates");
    let ended = base
        .iter()
        .filter(|event| event.kind() == EventKind::TrialEnd)
        .count();
    assert!(ended > 100, "{ended}");
}
