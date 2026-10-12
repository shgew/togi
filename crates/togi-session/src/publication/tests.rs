use super::*;
use crate::emergency::{Response, Withheld};
use crate::session::{Control, Stop};
use crate::testing::{Bench, kinds_of, swept};
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
    let saved: StateDocument = resumed.appender.journal().read_state().unwrap().unwrap();
    assert_eq!(saved.facts, resumed.appender.folds.facts);
    assert_eq!(saved.tuner, resumed.appender.tuner().project());
}

#[test]
fn projection_warning_append_failure_is_fatal() {
    for after_sweep in [false, true] {
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
    let held = resumed.held();
    resumed.reconcile(&held).unwrap();
    resumed.record_shutdown(&Stop::Signal);
    let ended = resumed.close(Ok(Stop::Signal));
    assert!(ended.errors.is_empty());
    assert!(matches!(
        ended.journal.events().last().unwrap().payload,
        Payload::Shutdown(_)
    ));
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
    let expected = StateDocument {
        facts: togi_journal::State::replay(ended.journal.events()).unwrap(),
        tuner: togi_tuner::replay(ended.journal.events())
            .unwrap()
            .project(),
    };
    let saved: StateDocument = ended.journal.read_state().unwrap().unwrap();
    assert_eq!(saved, expected);
    let bytes = serde_json::to_vec(&saved).unwrap();
    assert_eq!(
        serde_json::from_slice::<StateDocument>(&bytes).unwrap(),
        expected
    );
    let dir = ended.journal.close().unwrap();
    assert_eq!(
        dir.read_state::<StateDocument>().unwrap().unwrap(),
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
    let saved: StateDocument = ended.journal.read_state().unwrap().unwrap();
    assert_eq!(
        saved.facts,
        togi_journal::State::replay(ended.journal.events()).unwrap()
    );
}

#[test]
fn live_publishes_after_every_append_and_the_last_state_covers_the_final_event() {
    let bench = Bench::new(2);
    let mut input = bench.input();
    input.cancel.cancel();
    input.publication = PublicationPolicy::Live;
    let ended = crate::run(input);
    assert!(ended.errors.is_empty());
    assert!(publications(&bench) > ended.journal.events().len());
    let saved: StateDocument = ended.journal.read_state().unwrap().unwrap();
    assert_eq!(
        saved.facts,
        togi_journal::State::replay(ended.journal.events()).unwrap()
    );
    assert_eq!(
        saved.tuner,
        togi_tuner::replay(ended.journal.events())
            .unwrap()
            .project()
    );
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
