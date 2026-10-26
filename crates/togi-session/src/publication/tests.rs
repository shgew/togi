use super::*;
use crate::emergency::{Response, Withheld};
use crate::session::{Control, Stop};
use crate::testing::{Bench, kinds_of, swept};
use test_case::test_case;
use togi_journal::fs::{Fault, Op};

#[test]
fn state_write_failure_warns_continues_and_rebuilds() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    session.appender.publication = Some(PublicationPolicy::Live);
    assert!(session.appender.publish_state().unwrap());
    let old = bench.mem.contents("/state/state.json").unwrap();
    bench.mem.fail_state_publication();
    session.begin().unwrap();
    let events = session.appender.journal().events();
    let warnings: Vec<usize> = (0..events.len())
        .filter(|&at| matches!(events[at].payload, Payload::SessionWarning(_)))
        .collect();
    assert_eq!(warnings.len(), 1);
    let at = warnings[0];
    assert!(at > 0);
    assert_eq!(events[at].cause, [events[at - 1].seq]);
    assert!(session.appender.failure().is_none());
    session.appender.publication = Some(PublicationPolicy::OnFinish);
    let before_failed = bench.mem.contents("/state/state.json").unwrap();
    let last = session.appender.journal().events().last().unwrap().seq;
    bench.mem.fail_state_publication();
    assert!(!session.appender.publish_state().unwrap());
    let preserved = bench.mem.contents("/state/state.json").unwrap();
    assert_eq!(preserved, before_failed);
    assert_ne!(preserved, old);
    let warning = session.appender.journal().events().last().unwrap();
    assert!(matches!(warning.payload, Payload::SessionWarning(_)));
    assert_eq!(warning.cause, [last]);
    session.record_shutdown(&Stop::Signal);
    bench.mem.fail_state_publication();
    let ended = session.close(Ok(Stop::Signal));
    assert!(ended.errors.is_empty());
    assert_eq!(ended.stop, Some(Stop::Signal));
    assert_eq!(bench.mem.contents("/state/state.json").unwrap(), preserved);
    drop(ended);
    let mut resumed = bench.open();
    resumed.rebuild_state().unwrap();
    assert!(kinds_of(&resumed.appender).contains(&"state.rebuilt"));
    assert!(resumed.appender.publish_state().unwrap());
    let (_, saved): (_, StateDocument) = resumed
        .appender
        .journal()
        .dir()
        .read_snapshot()
        .unwrap()
        .unwrap();
    assert_eq!(saved.facts, resumed.appender.folds.facts);
    assert_eq!(saved.tuner, resumed.appender.tuner().project());
}

#[test_case(false; "false")]
#[test_case(true; "true")]
fn projection_warning_append_failure_is_fatal(after_sweep: bool) {
    let bench = Bench::new(2);
    let mut session = if after_sweep {
        swept(&bench)
    } else {
        let mut session = bench.open();
        session.start().unwrap();
        session
    };
    if after_sweep {
        session.validated = Some(session.host.validate_smu().unwrap());
    }
    bench.mem.fail_state_publication();
    bench
        .mem
        .inject(Fault::before(Op::Write, 1).named("events.jsonl"));
    let error = session.appender.publish_state().unwrap_err();
    let ended = session.close(Err(error.into()));
    assert!(matches!(
        ended.errors.first(),
        Some(SessionError::Journal(_))
    ));
    let emergency = ended.emergency.unwrap();
    if after_sweep {
        assert!(matches!(emergency.response, Response::Zeroed(_)));
    } else {
        assert!(matches!(
            emergency.response,
            Response::Withheld(Withheld::NoSweep)
        ));
    }
    assert!(
        !ended
            .journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::Shutdown(_)))
    );
}

#[test]
fn state_rebuild_write_failure_warns_and_continues() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    session.begin().unwrap();
    drop(session);
    let mut resumed = bench.open();
    bench.mem.fail_state_publication();
    resumed.rebuild_state().unwrap();
    let kinds = kinds_of(&resumed.appender);
    assert!(!kinds.contains(&"state.rebuilt"));
    assert!(kinds.contains(&"session.warning"));
    assert!(!kinds.contains(&"crash.detected"));
    assert_eq!(resumed.preflight().unwrap(), Control::Continue);
    assert_eq!(resumed.sweep().unwrap(), Control::Continue);
    resumed.reconcile().unwrap();
    resumed.record_shutdown(&Stop::Signal);
    let ended = resumed.close(Ok(Stop::Signal));
    assert!(ended.errors.is_empty());
    assert!(matches!(
        ended.journal.events().last().unwrap().payload,
        Payload::Shutdown(_)
    ));
}

#[test]
fn startup_readback_resolution_survives_replay_and_rebuilds_a_stale_open_snapshot() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    session.begin().unwrap();
    let receipt = session
        .appender
        .smu_intent(
            togi_journal::SmuCommand::Set {
                core: crate::testing::core(1),
                offset: togi_machine::TargetOffset::ZERO,
            },
            &[],
        )
        .unwrap();
    let intent = receipt.intent();
    assert!(session.appender.publish_state().unwrap());
    drop(session);

    let mut resumed = bench.open();
    assert_eq!(resumed.start().unwrap(), Control::Continue);
    assert_eq!(resumed.preflight().unwrap(), Control::Continue);
    assert_eq!(resumed.sweep().unwrap(), Control::Continue);
    assert_eq!(resumed.reconcile().unwrap(), Control::Continue);
    assert!(resumed.appender.folds.facts.open.is_empty());
    assert_eq!(
        resumed
            .appender
            .recovery()
            .latest_unmatched_intent(resumed.appender.journal().boot()),
        None
    );
    assert!(resumed.appender.journal().events().iter().any(|event| {
        matches!(event.payload, Payload::SmuReadback(_)) && event.cause == [intent]
    }));
    crate::testing::assert_fold_consistent(&resumed.appender);
    drop(resumed);

    let mut reopened = bench.open();
    reopened.rebuild_state().unwrap();
    let Payload::StateRebuilt(rebuilt) =
        &reopened.appender.journal().events().last().unwrap().payload
    else {
        panic!("the stale open-intent snapshot was not rebuilt");
    };
    assert!(rebuilt.fields.contains(&"last_seq".to_owned()));
    assert!(reopened.appender.folds.facts.open.is_empty());
    assert!(reopened.appender.publish_state().unwrap());
    let (_, saved): (_, StateDocument) = reopened
        .appender
        .journal()
        .dir()
        .read_snapshot()
        .unwrap()
        .unwrap();
    assert_eq!(saved.facts, reopened.appender.folds.facts);
    assert_eq!(saved.recovery, *reopened.appender.recovery());
    crate::testing::assert_fold_consistent(&reopened.appender);
}

#[test]
fn a_state_file_holding_the_document_as_an_array_is_rebuilt() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    session.begin().unwrap();
    drop(session);
    let mut resumed = bench.open();
    let positional = (
        resumed.appender.folds.facts.clone(),
        resumed.appender.tuner().project(),
    );
    resumed.appender.journal.write_state(&positional).unwrap();
    let written: serde_json::Value =
        serde_json::from_slice(&bench.mem.contents("/state/state.json").unwrap()).unwrap();
    assert_eq!(written.as_array().map(Vec::len), Some(2));
    assert!(
        resumed
            .appender
            .journal()
            .read_state::<StateDocument>()
            .is_err()
    );
    resumed.rebuild_state().unwrap();
    let rebuilt = resumed.appender.journal().events().last().unwrap();
    assert_eq!(
        rebuilt.payload,
        Payload::StateRebuilt(StateRebuilt {
            fields: vec!["state.json".into()],
        })
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&bench.mem.contents("/state/state.json").unwrap()).unwrap();
    assert!(saved.is_object());
}

#[test]
fn deferred_state_is_written_before_the_journal_leaves_the_run() {
    let bench = Bench::new(2);
    let mut input = bench.input();
    input.cancel.cancel();
    input.publication = PublicationPolicy::OnFinish;
    let ended = crate::run(input);
    assert!(ended.errors.is_empty());
    let expected = StateDocument::of(&Folds::replay(ended.journal.events()).unwrap());
    let (_, saved): (_, StateDocument) = ended.journal.dir().read_snapshot().unwrap().unwrap();
    assert_eq!(saved, expected);
    let bytes = serde_json::to_vec(&saved).unwrap();
    assert_eq!(
        serde_json::from_slice::<StateDocument>(&bytes).unwrap(),
        expected
    );
    let dir = ended.journal.close().unwrap();
    assert_eq!(
        dir.read_snapshot::<StateDocument>().unwrap().unwrap().1,
        expected
    );
}

fn publications(bench: &Bench) -> usize {
    bench
        .mem
        .log()
        .iter()
        .filter(|(op, path)| {
            *op == Op::Rename && path == std::path::Path::new("/state/.state.json.tmp")
        })
        .count()
}

#[test]
fn on_finish_publishes_the_state_once_after_the_final_event() {
    let bench = Bench::new(2);
    bench.mem.record_history();
    let mut input = bench.input();
    input.cancel.cancel();
    input.publication = PublicationPolicy::OnFinish;
    let ended = crate::run(input);
    assert!(ended.errors.is_empty());
    assert_eq!(publications(&bench), 1);
    assert!(matches!(
        ended.journal.events().last().unwrap().payload,
        Payload::Shutdown(_)
    ));
    let (_, saved): (_, StateDocument) = ended.journal.dir().read_snapshot().unwrap().unwrap();
    assert_eq!(
        saved.facts,
        togi_journal::State::replay(ended.journal.events()).unwrap()
    );
}

#[test]
fn live_publishes_after_every_append_and_the_last_state_covers_the_final_event() {
    let bench = Bench::new(2);
    bench.mem.record_history();
    let mut input = bench.input();
    input.cancel.cancel();
    input.publication = PublicationPolicy::Live;
    let ended = crate::run(input);
    assert!(ended.errors.is_empty());
    assert!(publications(&bench) > ended.journal.events().len());
    let (_, saved): (_, StateDocument) = ended.journal.dir().read_snapshot().unwrap().unwrap();
    assert_eq!(
        saved.facts,
        togi_journal::State::replay(ended.journal.events()).unwrap()
    );
    let mut tuner = togi_tuner::policy::State::new(togi_tuner::policy::Params::SPEC);
    for event in ended.journal.events() {
        tuner.fold(event).unwrap();
    }
    assert_eq!(saved.tuner, tuner.project());
}

#[test]
fn a_failed_final_publication_warns_after_the_shutdown_and_the_run_ends_clean() {
    let bench = Bench::new(2);
    let mut input = bench.input();
    input.cancel.cancel();
    input.publication = PublicationPolicy::OnFinish;
    bench.mem.fail_state_publication();
    let ended = crate::run(input);
    assert!(ended.errors.is_empty());
    assert!(kinds_of_journal(&ended.journal).contains(&"session.warning"));
    assert!(
        ended
            .journal
            .read_state::<StateDocument>()
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        ended.journal.events().last().unwrap().payload,
        Payload::SessionWarning(_)
    ));
}

fn kinds_of_journal(journal: &togi_journal::Journal) -> Vec<&'static str> {
    journal
        .events()
        .iter()
        .map(|event| event.kind().as_str())
        .collect()
}

fn rebuilt_after_tampering(tamper: impl FnOnce(&mut serde_json::Value)) -> Vec<String> {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    session.begin().unwrap();
    assert!(session.appender.publish_state().unwrap());
    drop(session);
    let mut state: serde_json::Value =
        serde_json::from_slice(&bench.mem.contents("/state/state.json").unwrap()).unwrap();
    tamper(&mut state);
    bench
        .mem
        .put("/state/state.json", serde_json::to_vec(&state).unwrap());
    let mut resumed = bench.open();
    resumed.rebuild_state().unwrap();
    let Payload::StateRebuilt(rebuilt) =
        &resumed.appender.journal().events().last().unwrap().payload
    else {
        panic!("no state.rebuilt was appended");
    };
    rebuilt.fields.clone()
}

fn reseal(state: &mut serde_json::Value) {
    let prefix = serde_json::from_value(state["snapshot"]["prefix"].clone()).unwrap();
    state.as_object_mut().unwrap().remove("snapshot");
    *state = togi_journal::snapshot::seal(prefix, state).unwrap();
}

#[test_case("/recovery/trials", serde_json::json!(999), "recovery"; "run recovery")]
#[test_case("/incremental/stop", serde_json::json!({}), "incremental"; "incremental tuner")]
#[test_case("/defects/excluded", serde_json::json!([1]), "defects"; "defect index")]
#[test_case("/presentation/facts/stopped", serde_json::json!(true), "presentation"; "watch end view")]
fn boot_replays_independently_even_when_a_wrong_full_fold_has_a_valid_checksum(
    pointer: &str,
    value: serde_json::Value,
    expected: &str,
) {
    let fields = rebuilt_after_tampering(|state| {
        *state.pointer_mut(pointer).unwrap() = value;
        reseal(state);
    });
    assert_eq!(fields, [expected]);
}

#[test_case(serde_json::json!([1, "pending"]); "sized recovery record array")]
#[test_case(serde_json::json!({"seq": 1, "detail": "pending", "future": true}); "unknown recovery record key")]
fn resealed_nested_reducer_shape_changes_are_discarded(value: serde_json::Value) {
    let fields = rebuilt_after_tampering(|state| {
        state["recovery"]["smu"] = value;
        reseal(state);
    });
    assert_eq!(fields, ["state.json"]);
}

#[test]
fn a_changed_payload_without_a_new_checksum_is_rebuilt() {
    assert_eq!(
        rebuilt_after_tampering(|state| {
            state["recovery"]["trials"] = serde_json::json!(999);
        }),
        ["state.json"]
    );
}

#[test]
fn another_build_is_discarded_and_recorded() {
    assert_eq!(
        rebuilt_after_tampering(|state| {
            state["snapshot"]["build"] = serde_json::json!("another-build");
        }),
        ["build"]
    );
}

#[test]
fn every_differing_facts_key_is_named_in_journal_order() {
    let fields = rebuilt_after_tampering(|state| {
        state["facts"]["cores"] = serde_json::json!([]);
        state["facts"]["last_seq"] = serde_json::json!(1);
        reseal(state);
    });
    assert_eq!(fields, ["cores", "last_seq"]);
}

#[test]
fn a_disagreeing_tuner_projection_is_named_apart_from_facts() {
    let fields = rebuilt_after_tampering(|state| {
        state["tuner"]["stopped"] = serde_json::json!(true);
        state["facts"]["last_seq"] = serde_json::json!(1);
        reseal(state);
    });
    assert_eq!(fields, ["last_seq", "tuner"]);
}

#[test]
fn a_snapshot_one_event_behind_is_rebuilt_naming_last_seq() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    session.begin().unwrap();
    assert!(session.appender.publish_state().unwrap());
    session
        .appender
        .append(
            Payload::SessionWarning(SessionWarning {
                operation: "x".into(),
                trial: None,
                error: "e".into(),
            }),
            &[],
        )
        .unwrap();
    drop(session);
    let mut resumed = bench.open();
    resumed.rebuild_state().unwrap();
    let rebuilt = resumed.appender.journal().events().last().unwrap();
    assert_eq!(
        rebuilt.payload,
        Payload::StateRebuilt(StateRebuilt {
            fields: vec!["last_seq".into()],
        })
    );
}

#[test]
fn a_resealed_prefix_stamp_mismatch_is_rebuilt_and_named() {
    assert_eq!(
        rebuilt_after_tampering(|state| {
            state["snapshot"]["prefix"]["sha256"] = serde_json::json!("wrong-prefix");
            reseal(state);
        }),
        ["snapshot"]
    );
}

#[test_case("/facts/last_seq", serde_json::json!(1); "prefix sequence and facts disagree")]
#[test_case("/recovery/trials", serde_json::json!(999); "restored fold differs")]
#[test_case("/tuner/stopped", serde_json::json!(true); "published projection differs")]
fn independent_snapshot_verification_reports_resealed_mismatches(
    pointer: &str,
    value: serde_json::Value,
) {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    session.begin().unwrap();
    assert!(session.appender.publish_state().unwrap());
    let mut state: serde_json::Value =
        serde_json::from_slice(&bench.mem.contents("/state/state.json").unwrap()).unwrap();
    *state.pointer_mut(pointer).unwrap() = value;
    reseal(&mut state);
    bench
        .mem
        .put("/state/state.json", serde_json::to_vec(&state).unwrap());
    assert_eq!(
        crate::verify_snapshot(
            session.appender.journal().dir(),
            session.appender.journal().events()
        )
        .unwrap(),
        crate::SnapshotStatus::Different
    );
}

#[test]
fn on_finish_and_live_publish_identical_final_snapshots_and_journals() {
    let run = |policy| {
        let bench = Bench::new(2);
        let mut input = bench.input();
        input.publication = policy;
        let cancel = input.cancel.clone();
        input.on_append = Some(Box::new(move |event| {
            if matches!(event.payload, Payload::TrialEnd(_)) {
                cancel.cancel();
            }
        }));
        let ended = crate::run(input);
        assert!(ended.errors.is_empty(), "{:?}", ended.errors);
        let events = ended.journal.events().to_vec();
        let mut deferred = Folds::default();
        let mut live = Folds::default();
        for (at, event) in events.iter().enumerate() {
            deferred.apply_deferred(event).unwrap();
            live.apply(event).unwrap();
            if matches!(event.payload, Payload::TrialEnd(_)) {
                assert_eq!(deferred.presentation.deferred_end(), Some(event.seq));
                let mut published = deferred.clone();
                published.materialize(&events[..=at]).unwrap();
                assert_eq!(published, live);
            }
        }
        assert_eq!(deferred.presentation.deferred_end(), None);
        assert_eq!(deferred, live);
        assert!(
            events
                .iter()
                .any(|event| matches!(event.payload, Payload::TrialEnd(_)))
        );
        (events, bench.mem.contents("/state/state.json").unwrap())
    };
    assert_eq!(
        run(PublicationPolicy::OnFinish),
        run(PublicationPolicy::Live)
    );
}
