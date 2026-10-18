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

fn kinds(journal: &Journal) -> Vec<&'static str> {
    journal
        .events()
        .iter()
        .map(|event| event.kind().as_str())
        .collect()
}

#[test]
fn resetting_a_core_appends_only_the_command_and_no_shutdown() {
    let bench = Bench::new(2);
    let mut journal = journal_after_a_run(&bench);
    let before = kinds(&journal).len();
    let seq = reset_core(
        &build(),
        &mut journal,
        &mut *bench.sim.seams().clock,
        core(1),
    )
    .unwrap();
    let kinds = kinds(&journal);
    assert_eq!(kinds[before..], ["command.reset"]);
    let event = journal.event(seq).unwrap();
    assert!(matches!(
        &event.payload,
        Payload::CommandReset(reset) if reset.target == ResetTarget::Core(core(1))
    ));
    assert!(event.cause.is_empty());
}

#[test]
fn resetting_a_core_the_session_lacks_is_refused_and_appends_nothing() {
    let bench = Bench::new(2);
    let mut journal = journal_after_a_run(&bench);
    let before = kinds(&journal);
    let error = reset_core(
        &build(),
        &mut journal,
        &mut *bench.sim.seams().clock,
        core(9),
    )
    .unwrap_err();
    assert!(matches!(error, CommandError::NoSuchCore { core: lacking } if lacking == core(9)));
    assert_eq!(kinds(&journal), before);
}

#[test]
fn resetting_in_an_empty_journal_is_no_session() {
    let bench = Bench::new(2);
    let Session { appender, .. } = bench.open();
    let mut journal = appender.into_journal();
    let error = reset_core(
        &build(),
        &mut journal,
        &mut *bench.sim.seams().clock,
        core(0),
    )
    .unwrap_err();
    assert!(matches!(error, CommandError::NoSession));
}

#[test]
fn resetting_with_another_build_is_refused() {
    let bench = Bench::new(2);
    let mut journal = journal_after_a_run(&bench);
    let mut other = build();
    other.schema += 1;
    let before = kinds(&journal);
    let error =
        reset_core(&other, &mut journal, &mut *bench.sim.seams().clock, core(0)).unwrap_err();
    assert!(matches!(error, CommandError::Compatibility(_)));
    assert_eq!(kinds(&journal), before);
}

#[test]
fn resetting_everything_records_the_command_and_archives_the_session() {
    let bench = Bench::new(2);
    let journal = journal_after_a_run(&bench);
    let (session, dir) = reset_all(&build(), journal, &mut *bench.sim.seams().clock).unwrap();
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
    assert!(session.appender.recovery().phase(core(1)).is_none());
    let mut journal = session.appender.into_journal();
    let before = kinds(&journal).len();
    reset_core(
        &build(),
        &mut journal,
        &mut *bench.sim.seams().clock,
        core(1),
    )
    .unwrap();
    assert_eq!(kinds(&journal)[before..], ["command.reset"]);
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
}

fn unfoldable(bench: &Bench) -> Journal {
    let mut journal = journal_after_a_run(bench);
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
            stamp(&*bench.sim.seams().clock),
            Payload::CorePhase(transition),
            &[],
        )
        .unwrap();
    assert!(Folds::replay(journal.events()).is_err());
    journal
}

#[test]
fn resetting_a_core_of_a_journal_that_cannot_be_folded_is_a_fold_error_and_appends_nothing() {
    let bench = Bench::new(2);
    let mut journal = unfoldable(&bench);
    let before = kinds(&journal);
    let error = reset_core(
        &build(),
        &mut journal,
        &mut *bench.sim.seams().clock,
        core(0),
    )
    .unwrap_err();
    assert!(matches!(error, CommandError::Fold(_)));
    assert_eq!(kinds(&journal), before);
}

#[test]
fn resetting_everything_archives_a_journal_that_cannot_be_folded_without_replaying_it() {
    let bench = Bench::new(2);
    let journal = unfoldable(&bench);
    let (session, dir) = reset_all(&build(), journal, &mut *bench.sim.seams().clock).unwrap();
    assert_eq!(dir.archives().unwrap(), [session]);
}

#[test]
fn resetting_a_core_with_another_build_is_refused_before_the_journal_is_folded() {
    let bench = Bench::new(2);
    let mut journal = unfoldable(&bench);
    let mut other = build();
    other.ruleset += 1;
    let error =
        reset_core(&other, &mut journal, &mut *bench.sim.seams().clock, core(0)).unwrap_err();
    assert!(matches!(error, CommandError::Compatibility(_)));
}
