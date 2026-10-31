use test_case::test_case;
use togi_journal::{Journal, ResetTarget, SessionId};

use super::*;
use crate::session::{Control, Session};
use crate::testing::{Bench, build, core, simulated};

fn journal_after_a_run(bench: &Bench) -> Journal {
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    session.appender.into_journal()
}

#[test]
fn resetting_everything_records_the_command_and_archives_the_session() {
    let bench = Bench::new(2);
    let journal = journal_after_a_run(&bench);
    let (session, dir) = reset_all(&build(), journal, &mut *bench.sim.seams().clock).unwrap();
    let archived: Vec<SessionId> = dir.archives().unwrap();
    assert_eq!(archived, [session]);
    let recording = dir.archived_recording(&session).unwrap().unwrap();
    let command = recording.events().iter().find_map(|event| {
        if let Payload::CommandReset(reset) = &event.payload {
            Some(reset.target)
        } else {
            None
        }
    });
    assert_eq!(command, Some(ResetTarget::All));
    drop(dir);
    assert_eq!(simulated(&bench.mem).archives().unwrap(), [session]);
}

#[test]
fn resetting_everything_makes_the_boundary_permanent_and_leaves_no_import_marker() {
    let bench = Bench::new(2);
    let journal = journal_after_a_run(&bench);
    let (session, dir) = reset_all(&build(), journal, &mut *bench.sim.seams().clock).unwrap();
    assert_eq!(dir.reset_boundary().unwrap(), Some(session));
    assert_eq!(dir.pending_import().unwrap(), None);
}

#[test]
fn resetting_in_an_empty_journal_is_no_session() {
    let bench = Bench::new(2);
    let Ok(Session { appender, .. }) = Session::open(bench.input_unattested()) else {
        panic!("open empty journal");
    };
    let journal = appender.into_journal();
    let error = reset_all(&build(), journal, &mut *bench.sim.seams().clock).unwrap_err();
    assert!(matches!(error, CommandError::NoSession));
}

#[test_case("ruleset", |build| build.ruleset += 1; "ruleset")]
#[test_case("evidence", |build| build.evidence += 1; "evidence")]
#[test_case("schema", |build| build.schema += 1; "schema")]
fn resetting_everything_archives_across_a_ruleset_an_evidence_epoch_or_a_schema_mismatch(
    name: &str,
    change: fn(&mut Build),
) {
    let bench = Bench::new(2);
    let journal = journal_after_a_run(&bench);
    let mut other = build();
    change(&mut other);
    let (session, dir) = reset_all(&other, journal, &mut *bench.sim.seams().clock).expect(name);
    assert_eq!(dir.archives().unwrap(), [session], "{name}");
    assert_eq!(dir.reset_boundary().unwrap(), Some(session), "{name}");
}

#[test]
fn resetting_everything_archives_a_journal_that_cannot_be_folded_without_replaying_it() {
    let bench = Bench::new(2);
    let mut journal = journal_after_a_run(&bench);
    let transition = togi_journal::CoreTransition {
        core: core(9),
        from: None,
        to: togi_journal::CorePhase::AtLimit,
        offset: crate::testing::target(-6),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".into(),
    };
    journal
        .append(
            stamp(&*bench.sim.seams().clock).unwrap(),
            Payload::CorePhase(transition),
            &[],
        )
        .unwrap();
    assert!(crate::appender::Folds::replay(journal.events()).is_err());
    let (session, dir) = reset_all(&build(), journal, &mut *bench.sim.seams().clock).unwrap();
    assert_eq!(dir.archives().unwrap(), [session]);
}
