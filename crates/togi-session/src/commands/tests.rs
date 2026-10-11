use togi_journal::{ResetTarget, SessionId};

use super::*;
use crate::session::{Control, Session};
use crate::testing::{Bench, build, core, kinds_of, simulated};

fn appender_after_a_run(bench: &Bench) -> Appender {
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    let Session { appender, .. } = session;
    appender
}

fn kinds(appender: &Appender) -> Vec<&'static str> {
    kinds_of(appender)
}

#[test]
fn resetting_a_core_appends_only_the_command_and_no_shutdown() {
    let bench = Bench::new(2);
    let mut appender = appender_after_a_run(&bench);
    let before = kinds(&appender).len();
    let seq = reset_core(&build(), &mut appender, core(1)).unwrap();
    let kinds = kinds(&appender);
    assert_eq!(kinds[before..], ["command.reset"]);
    let event = appender.journal().event(seq).unwrap();
    assert!(matches!(
        &event.payload,
        Payload::CommandReset(reset) if reset.target == ResetTarget::Core(core(1))
    ));
    assert!(event.cause.is_empty());
}

#[test]
fn resetting_a_core_the_session_lacks_is_refused_and_appends_nothing() {
    let bench = Bench::new(2);
    let mut appender = appender_after_a_run(&bench);
    let before = kinds(&appender);
    let error = reset_core(&build(), &mut appender, core(9)).unwrap_err();
    assert!(matches!(error, CommandError::NoSuchCore { core: lacking } if lacking == core(9)));
    assert_eq!(kinds(&appender), before);
}

#[test]
fn resetting_in_an_empty_journal_is_no_session() {
    let bench = Bench::new(2);
    let session = bench.open();
    let Session { mut appender, .. } = session;
    let error = reset_core(&build(), &mut appender, core(0)).unwrap_err();
    assert!(matches!(error, CommandError::NoSession));
}

#[test]
fn resetting_with_another_build_is_refused() {
    let bench = Bench::new(2);
    let mut appender = appender_after_a_run(&bench);
    let mut other = build();
    other.schema += 1;
    let before = kinds(&appender);
    let error = reset_core(&other, &mut appender, core(0)).unwrap_err();
    assert!(matches!(error, CommandError::Compatibility(_)));
    assert_eq!(kinds(&appender), before);
}

#[test]
fn resetting_everything_records_the_command_and_archives_the_session() {
    let bench = Bench::new(2);
    let appender = appender_after_a_run(&bench);
    let (session, dir) = reset_all(&build(), appender).unwrap();
    let archived: Vec<SessionId> = dir.archives().unwrap();
    assert_eq!(archived, [session]);
    drop(dir);
    assert_eq!(simulated(&bench.mem).archives().unwrap(), [session]);
}

#[test]
fn a_core_of_a_session_interrupted_before_its_phases_can_still_be_reset() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    let Session { mut appender, .. } = session;
    assert!(appender.recovery().phase(core(1)).is_none());
    let before = kinds(&appender).len();
    reset_core(&build(), &mut appender, core(1)).unwrap();
    assert_eq!(kinds(&appender)[before..], ["command.reset"]);
}

#[test]
fn resetting_everything_archives_across_a_ruleset_an_evidence_epoch_or_a_schema_mismatch() {
    type Change = fn(&mut Build);
    let changes: [(&str, Change); 3] = [
        ("ruleset", |build| build.ruleset += 1),
        ("evidence", |build| build.evidence += 1),
        ("schema", |build| build.schema += 1),
    ];
    for (name, change) in changes {
        let bench = Bench::new(2);
        let appender = appender_after_a_run(&bench);
        let mut other = build();
        change(&mut other);
        let (session, dir) = reset_all(&other, appender).expect(name);
        assert_eq!(dir.archives().unwrap(), [session], "{name}");
    }
}
