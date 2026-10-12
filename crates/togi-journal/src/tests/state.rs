use super::fixtures::{at, core, end, intent, sample, samples};
use crate::*;

fn start() -> Event {
    at(1, "boot-a", 0, &[], sample(EventKind::SessionStart))
}

fn smu_intent(seq_no: u64, boot: &str) -> Event {
    at(seq_no, boot, 1, &[], sample(EventKind::SmuIntent))
}

fn smu_write(seq_no: u64, boot: &str, cause: u64) -> Event {
    at(seq_no, boot, 1, &[cause], sample(EventKind::SmuWrite))
}

fn replay(events: &[Event]) -> Result<State, FoldError> {
    State::replay(events)
}

#[test]
fn start_records_the_session_and_sorted_cores() {
    let state = replay(&[start()]).unwrap();
    let session = state.session.as_ref().unwrap();
    assert_eq!(session.id.to_string(), "20261002T011407Z");
    assert_eq!(session.build.schema, SCHEMA);
    assert_eq!(state.schema, SCHEMA);
    assert_eq!(state.last_seq, Seq::new(1));
    let cores: Vec<_> = state.cores.iter().map(|facts| facts.core).collect();
    assert_eq!(cores, [core(0), core(1)]);
}

#[test]
fn context_and_baseline_are_recorded_per_core() {
    let state = replay(&[
        start(),
        at(2, "boot-a", 1, &[], sample(EventKind::SessionContext)),
        at(3, "boot-a", 2, &[], sample(EventKind::SessionBaseline)),
    ])
    .unwrap();
    let session = state.session.as_ref().unwrap();
    assert_eq!(session.bios_context.as_ref().unwrap().bios_version, "F31");
    assert_eq!(session.baseline.len(), 2);
    assert_eq!(
        state.core(core(1)).unwrap().baseline,
        Some(togi_machine::ObservedOffset(-5))
    );
    assert_eq!(
        state.core(core(0)).unwrap().baseline,
        Some(togi_machine::ObservedOffset(0))
    );
}

#[test]
fn decisions_and_core_phases_set_the_last_decision() {
    let Payload::TunerDecision(mut decision) = sample(EventKind::TunerDecision) else {
        unreachable!()
    };
    decision.core = core(1);
    let state = replay(&[start(), at(2, "boot-a", 1, &[], decision.into())]).unwrap();
    let last = state.core(core(1)).unwrap().last_decision.as_ref().unwrap();
    assert_eq!(last.seq, Seq::new(2).unwrap());
    assert!(last.msg.starts_with("core 01"));
    assert!(state.core(core(0)).unwrap().last_decision.is_none());
}

#[test]
fn dead_end_is_recorded_and_cleared_by_config_loaded() {
    let Payload::DeadEnd(mut dead_end) = sample(EventKind::DeadEnd) else {
        unreachable!()
    };
    dead_end.core = Some(core(0));
    let Payload::TunerDecision(mut decision) = sample(EventKind::TunerDecision) else {
        unreachable!()
    };
    decision.core = core(0);
    let dead_end_event = at(3, "boot-a", 2, &[], dead_end.into());
    let mut events = vec![
        start(),
        at(2, "boot-a", 1, &[], decision.into()),
        dead_end_event.clone(),
    ];
    let state = replay(&events).unwrap();
    let reference = state.dead_end.unwrap();
    assert_eq!(reference.condition, DeadEndCondition::FailureAtZero);
    assert_eq!(reference.seq, Seq::new(3).unwrap());
    assert_eq!(
        state.core(core(0)).unwrap().last_decision,
        Some(DecisionRef {
            seq: dead_end_event.seq,
            msg: dead_end_event.msg,
        })
    );
    events.push(at(4, "boot-a", 3, &[], sample(EventKind::ConfigLoaded)));
    assert_eq!(replay(&events).unwrap().dead_end, None);
}

#[test]
fn a_core_phase_replaces_the_earlier_last_decision_with_its_own_seq_and_msg() {
    let Payload::TunerDecision(mut decision) = sample(EventKind::TunerDecision) else {
        unreachable!()
    };
    decision.core = core(1);
    let Payload::CorePhase(mut phase) = sample(EventKind::CorePhase) else {
        unreachable!()
    };
    phase.core = core(1);
    let phase_event = at(3, "boot-a", 2, &[], phase.into());
    let state = replay(&[
        start(),
        at(2, "boot-a", 1, &[], decision.into()),
        phase_event.clone(),
    ])
    .unwrap();
    assert_eq!(
        state.core(core(1)).unwrap().last_decision,
        Some(DecisionRef {
            seq: phase_event.seq,
            msg: phase_event.msg,
        })
    );
    assert!(state.core(core(0)).unwrap().last_decision.is_none());
}

#[test]
fn config_loaded_refreshes_only_version_rev_and_fixes() {
    let Payload::ConfigLoaded(mut config) = sample(EventKind::ConfigLoaded) else {
        unreachable!()
    };
    config.build.version = "9.9.9".into();
    config.build.fixes = 7;
    config.build.ruleset = 99;
    config.build.evidence = 99;
    let state = replay(&[start(), at(2, "boot-a", 1, &[], config.into())]).unwrap();
    let build = &state.session.unwrap().build;
    assert_eq!((build.version.as_str(), build.fixes), ("9.9.9", 7));
    assert_eq!((build.ruleset, build.evidence), (10, 1));
}

#[test]
fn a_new_smu_intent_replaces_the_older_one_and_the_write_citing_it_clears_it() {
    let state = replay(&[start(), smu_intent(2, "boot-a"), smu_intent(3, "boot-a")]).unwrap();
    assert_eq!(state.open.len(), 1);
    assert_eq!(state.in_flight().unwrap().seq, Seq::new(3).unwrap());
    let state = replay(&[start(), smu_intent(2, "boot-a"), smu_write(3, "boot-a", 2)]).unwrap();
    assert!(state.in_flight().is_none());
}

#[test]
fn a_write_citing_another_intent_leaves_this_one_open() {
    let state = replay(&[start(), smu_intent(2, "boot-a"), smu_write(3, "boot-a", 1)]).unwrap();
    assert_eq!(state.in_flight().unwrap().seq, Seq::new(2).unwrap());
}

#[test]
fn smu_error_clears_the_intent_it_cites() {
    let error = at(3, "boot-a", 1, &[2], sample(EventKind::SmuError));
    let state = replay(&[start(), smu_intent(2, "boot-a"), error]).unwrap();
    assert!(state.in_flight().is_none());
}

#[test]
fn trial_end_closes_only_its_own_trial() {
    let state = replay(&[
        start(),
        at(2, "boot-a", 1, &[], intent("0001", Some(0), &[])),
        at(3, "boot-a", 2, &[], intent("0002", Some(1), &[])),
        at(4, "boot-a", 3, &[3], end("0001", None)),
    ])
    .unwrap();
    assert_eq!(state.open.len(), 1);
    let open = state.in_flight().unwrap();
    assert_eq!(open.trial.as_ref().unwrap().as_str(), "0002");
    assert_eq!(open.kind, EventKind::TrialIntent);
}

#[test]
fn the_newest_unmatched_intent_is_in_flight_across_kinds() {
    let state = replay(&[
        start(),
        at(2, "boot-a", 1, &[], intent("0001", Some(0), &[])),
        smu_intent(3, "boot-a"),
    ])
    .unwrap();
    assert_eq!(state.open.len(), 2);
    assert_eq!(state.in_flight().unwrap().kind, EventKind::SmuIntent);
}

#[test]
fn completing_the_smu_command_leaves_the_open_trial_in_flight() {
    let state = replay(&[
        start(),
        at(2, "boot-a", 1, &[], intent("0001", Some(0), &[])),
        smu_intent(3, "boot-a"),
        smu_write(4, "boot-a", 3),
    ])
    .unwrap();
    assert_eq!(state.open.len(), 1);
    let open = state.in_flight().unwrap();
    assert_eq!(open.kind, EventKind::TrialIntent);
    assert_eq!(open.seq, Seq::new(2).unwrap());
    assert_eq!(open.trial.as_ref().unwrap().as_str(), "0001");
}

#[test]
fn crash_detected_clears_only_the_previous_boots_intents() {
    let Payload::CrashDetected(mut crash) = sample(EventKind::CrashDetected) else {
        unreachable!()
    };
    crash.previous_boot = togi_machine::BootId::from("boot-a".to_owned());
    let state = replay(&[
        start(),
        at(2, "boot-a", 1, &[], intent("0001", Some(0), &[])),
        at(3, "boot-b", 2, &[], intent("0002", Some(1), &[])),
        at(4, "boot-b", 3, &[], crash.into()),
    ])
    .unwrap();
    assert_eq!(state.open.len(), 1);
    assert_eq!(state.in_flight().unwrap().boot.as_str(), "boot-b");
}

#[test]
fn events_without_shared_facts_only_advance_last_seq() {
    let mut events = vec![start()];
    for (index, payload) in samples().into_iter().enumerate().skip(1) {
        if matches!(
            payload.kind(),
            EventKind::SessionStart
                | EventKind::SessionBaseline
                | EventKind::TunerDecision
                | EventKind::CorePhase
                | EventKind::DeadEnd
        ) {
            continue;
        }
        let seq_no = u64::try_from(events.len()).unwrap() + 1;
        let _ = index;
        events.push(at(seq_no, "boot-a", 1, &[], payload));
    }
    let state = replay(&events).unwrap();
    assert_eq!(
        state.last_seq,
        Seq::new(u64::try_from(events.len()).unwrap())
    );
}

#[test]
fn an_event_before_session_start_is_a_fold_error() {
    let error = replay(&[at(1, "boot-a", 0, &[], sample(EventKind::SessionContext))]).unwrap_err();
    assert_eq!(
        error,
        FoldError::BeforeStart {
            seq: Seq::FIRST,
            kind: EventKind::SessionContext
        }
    );
}

#[test]
fn a_second_start_is_a_fold_error() {
    let second = at(2, "boot-a", 1, &[], sample(EventKind::SessionStart));
    assert_eq!(
        replay(&[start(), second]).unwrap_err(),
        FoldError::SecondStart {
            seq: Seq::new(2).unwrap()
        }
    );
}

#[test]
fn an_event_that_does_not_come_after_the_last_is_a_fold_error_and_a_hole_is_not() {
    let hole = at(3, "boot-a", 1, &[], sample(EventKind::SessionContext));
    let state = replay(&[start(), hole]).unwrap();
    assert_eq!(state.last_seq, Seq::new(3));
    let again = at(1, "boot-a", 1, &[], sample(EventKind::SessionContext));
    assert_eq!(
        replay(&[start(), again]).unwrap_err(),
        FoldError::Gap {
            seq: Seq::FIRST,
            after: Some(Seq::FIRST)
        }
    );
    let late = at(2, "boot-a", 0, &[], sample(EventKind::SessionStart));
    assert_eq!(
        replay(&[late]).unwrap_err(),
        FoldError::Gap {
            seq: Seq::new(2).unwrap(),
            after: None
        }
    );
}

#[test]
fn decisions_and_baselines_for_unknown_cores_are_fold_errors() {
    let Payload::TunerDecision(mut decision) = sample(EventKind::TunerDecision) else {
        unreachable!()
    };
    decision.core = core(9);
    let error = replay(&[start(), at(2, "boot-a", 1, &[], decision.into())]).unwrap_err();
    assert!(matches!(error, FoldError::UnknownCore { core: unknown, .. } if unknown == core(9)));
    let Payload::SessionBaseline(mut baseline) = sample(EventKind::SessionBaseline) else {
        unreachable!()
    };
    baseline
        .offsets
        .set(core(9), togi_machine::ObservedOffset(0));
    let error = replay(&[start(), at(2, "boot-a", 1, &[], baseline.into())]).unwrap_err();
    assert!(matches!(error, FoldError::UnknownCore { .. }));
}

#[test]
fn state_round_trips_through_json_and_compares_equal() {
    let state = replay(&[
        start(),
        at(2, "boot-a", 1, &[], sample(EventKind::SessionContext)),
        at(3, "boot-a", 2, &[], sample(EventKind::SessionBaseline)),
        at(4, "boot-a", 3, &[], intent("0001", Some(0), &[])),
    ])
    .unwrap();
    let json = serde_json::to_string(&state).unwrap();
    let back: State = serde_json::from_str(&json).unwrap();
    assert_eq!(back, state);
}

#[test]
fn a_prefix_state_equals_the_state_built_event_by_event() {
    let events = [
        start(),
        at(2, "boot-a", 1, &[], intent("0001", Some(0), &[])),
        smu_intent(3, "boot-a"),
    ];
    let mut incremental = State::default();
    for event in &events {
        incremental.apply(event).unwrap();
    }
    assert_eq!(incremental, replay(&events).unwrap());
}

#[test]
fn backend_paths_follow_the_latest_config_loaded() {
    let state = replay(&[
        start(),
        at(2, "boot-a", 1, &[], sample(EventKind::ConfigLoaded)),
    ])
    .unwrap();
    assert_eq!(
        state.backends.path(togi_machine::Backend::Mprime),
        Some(std::path::Path::new("/nix/store/x-mprime"))
    );
    assert_eq!(state.backends.path(togi_machine::Backend::Ycruncher), None);
    let Payload::ConfigLoaded(mut config) = sample(EventKind::ConfigLoaded) else {
        unreachable!()
    };
    config.config = ConfigSnapshot {
        backends: ConfigBackends {
            mprime: None,
            ycruncher: Some("/nix/store/y".into()),
        },
        ..super::fixtures::bare_config()
    };
    let state = replay(&[
        start(),
        at(2, "boot-a", 1, &[], sample(EventKind::ConfigLoaded)),
        at(3, "boot-a", 2, &[], config.into()),
    ])
    .unwrap();
    assert_eq!(state.backends.path(togi_machine::Backend::Mprime), None);
    assert_eq!(
        state.backends.path(togi_machine::Backend::Ycruncher),
        Some(std::path::Path::new("/nix/store/y"))
    );
}
