use togi_defect::{DefectState, Entry, KNOWN_DEFECTS};
use togi_journal::{DefectDirection, DefectId, Payload};
use togi_machine::Signal;
use togi_sim::MachineDefinition;
use togi_sim::definition::ModelDef;

use super::*;
use crate::testing::Bench;
use crate::{Ended, Stop, run};

static CAUTIOUS: [Entry; 1] = [entry(DefectDirection::TooCautious)];
static AGGRESSIVE: [Entry; 1] = [entry(DefectDirection::TooAggressive)];

const fn entry(direction: DefectDirection) -> Entry {
    Entry {
        id: DefectId::new(1).unwrap(),
        title: "synthetic",
        detail: "synthetic detail",
        pr: 100,
        direction,
        clauses: &[],
    }
}

fn bench() -> Bench {
    Bench::defined(&MachineDefinition {
        cores: 2,
        model: ModelDef {
            past_limit_rate: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    })
}

fn one_cycle(bench: &Bench, resume: Option<ResumeCapsule>) -> Ended {
    let mut input = bench.input_ending_at(bench.short_night());
    input.resume = resume;
    run(input)
}

fn capsule_of(journal: &Journal, defects: &'static [Entry]) -> ResumeCapsule {
    let folds = Folds::replay_with(defects, journal.events()).unwrap();
    ResumeCapsule::new(journal, defects, folds)
}

fn fold_of(events: &[Event]) -> Folds {
    Folds::replay_with(KNOWN_DEFECTS, events).unwrap()
}

fn halves(ended: &Ended) -> (&[Event], &[Event]) {
    let events = ended.journal.events();
    (&events[..events.len() / 2], events)
}

#[test]
fn a_capsule_verifies_the_state_it_folded() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    assert!(matches!(ended.stop, Some(Stop::Signal)), "{:?}", ended.stop);
    let capsule = ended.capsule.as_ref().unwrap();
    capsule.verify(ended.journal.events()).unwrap();
    let replayed = crate::ReadState::replay(ended.journal.events()).unwrap();
    assert_eq!(replayed.facts(), &capsule.folds.facts);
    assert_eq!(replayed.tuner(), &capsule.folds.tuner);
}
#[test]
fn verify_names_the_part_that_differs() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    let (prefix, events) = halves(&ended);
    let behind = fold_of(prefix);

    let mut facts = capsule_of(&ended.journal, KNOWN_DEFECTS);
    facts.folds.facts = behind.facts.clone();
    assert!(matches!(facts.verify(events), Err(Divergence::Facts)));

    let mut tuner = capsule_of(&ended.journal, KNOWN_DEFECTS);
    tuner.folds.tuner = behind.tuner.clone();
    assert!(matches!(tuner.verify(events), Err(Divergence::Tuner)));

    let mut recovery = capsule_of(&ended.journal, KNOWN_DEFECTS);
    recovery.folds.recovery = behind.recovery;
    assert!(matches!(recovery.verify(events), Err(Divergence::Recovery)));

    let mut defects = capsule_of(&ended.journal, &CAUTIOUS);
    defects.folds.defects = DefectState::new(&AGGRESSIVE);
    assert!(matches!(defects.verify(events), Err(Divergence::Defects)));

    let mut presentation = capsule_of(&ended.journal, KNOWN_DEFECTS);
    presentation.folds.presentation = behind.presentation;
    assert!(matches!(
        presentation.verify(events),
        Err(Divergence::Presentation)
    ));
}
#[test]
fn verify_rejects_a_capsule_that_misses_events() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    let (prefix, events) = halves(&ended);
    let mut capsule = capsule_of(&ended.journal, KNOWN_DEFECTS);
    capsule.last = last_seq(prefix);
    assert!(matches!(
        capsule.verify(events),
        Err(Divergence::Last { .. })
    ));
}

#[test]
fn a_capsule_is_adopted_only_when_it_covers_exactly_the_journal() {
    let bench = bench();
    let Ended {
        mut journal,
        capsule,
        ..
    } = one_cycle(&bench, None);
    let capsule = capsule.unwrap();
    assert!(capsule.clone().adopt(&journal, bench.defects).is_some());

    journal.reboot("rebooted".to_owned().into()).unwrap();
    assert!(
        capsule.covers(&journal, bench.defects),
        "same open journal across a reboot"
    );

    let dir = journal.close().unwrap();
    let mut journal = dir.open("reopened".to_owned().into()).unwrap();
    assert!(
        !capsule.covers(&journal, bench.defects),
        "fresh open at an unchanged end"
    );
    let current = capsule_of(&journal, bench.defects);
    let last = journal.events().last().unwrap();
    let stamp = togi_journal::Stamp {
        time: last.time,
        mono: None,
    };
    journal
        .append(
            stamp,
            Payload::SessionWarning(togi_journal::SessionWarning {
                operation: "test".into(),
                trial: None,
                error: "test".into(),
            }),
            &[],
        )
        .unwrap();
    assert!(
        !current.covers(&journal, bench.defects),
        "same open journal with a longer tail"
    );
}

#[test]
fn a_capsule_is_not_adopted_by_a_run_judging_by_other_defects() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    let capsule = ended.capsule.clone().unwrap();
    assert!(capsule.adopt(&ended.journal, &CAUTIOUS).is_none());
}

fn journal_text(ended: &Ended) -> Vec<String> {
    ended
        .journal
        .events()
        .iter()
        .map(|event| format!("{event:?}"))
        .collect()
}

#[test]
fn a_run_given_the_capsule_of_another_journal_folds_its_own() {
    let donor = bench();
    let capsule = one_cycle(&donor, None).capsule.unwrap();

    let own = bench();
    let ended = one_cycle(&own, Some(capsule));
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);

    let plain = bench();
    let expected = one_cycle(&plain, None);
    assert_eq!(journal_text(&ended), journal_text(&expected));
}

fn crashed(bench: &Bench) -> Ended {
    bench
        .sim
        .script_trial(
            "0001",
            togi_sim::model::Script {
                signal: Some(Signal::ComputationError),
                at_s: 1.0,
                core: crate::testing::core(0),
                reset: None,
                then_crash: true,
            },
        )
        .unwrap();
    let input = bench.input_ending_at(bench.short_night());
    let ended = run(input);
    assert!(
        ended.errors.iter().any(crate::SessionError::is_crash),
        "{:?}",
        ended.errors
    );
    ended
}

#[test]
fn a_crash_returns_the_folds_of_its_durable_events() {
    let bench = bench();
    let ended = crashed(&bench);
    ended
        .capsule
        .as_ref()
        .unwrap()
        .verify(ended.journal.events())
        .unwrap();
}

#[test]
fn the_boot_after_a_crash_resumes_the_same_from_a_capsule_or_a_replay() {
    let warm = bench();
    let Ended {
        journal, capsule, ..
    } = crashed(&warm);
    warm.sim.reboot().unwrap();
    let resumed = run(warm.continuing_at(journal, capsule, warm.short_night()));

    let cold = bench();
    drop(crashed(&cold).journal.close().unwrap());
    cold.sim.reboot().unwrap();
    let replayed = one_cycle(&cold, None);

    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    assert_eq!(resumed.stop, replayed.stop);
    assert_eq!(journal_text(&resumed), journal_text(&replayed));
    resumed
        .capsule
        .unwrap()
        .verify(resumed.journal.events())
        .unwrap();
}

#[test]
fn cold_resume_ignores_legacy_state_without_reading_or_rewriting_it() {
    let cold = bench();
    drop(crashed(&cold).journal.close().unwrap());
    cold.sim.reboot().unwrap();
    let leftover = br#"{"snapshot":{"build":"foreign"}"#;
    cold.mem.put("/state/state.json", leftover);
    cold.mem.put("/state/.state.json.tmp", b"partial");
    cold.mem.record_history();

    let resumed = one_cycle(&cold, None);
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    resumed
        .capsule
        .as_ref()
        .unwrap()
        .verify(resumed.journal.events())
        .unwrap();
    assert!(
        resumed
            .journal
            .events()
            .iter()
            .all(|event| !matches!(event.payload, Payload::StateRebuilt(_)))
    );
    let is_legacy =
        |path: &std::path::Path| path.ends_with("state.json") || path.ends_with(".state.json.tmp");
    assert!(!cold.mem.reads().iter().any(|path| is_legacy(path)));
    assert!(!cold.mem.log().iter().any(|(_, path)| is_legacy(path)));
    assert_eq!(
        cold.mem.contents("/state/state.json").as_deref(),
        Some(leftover.as_slice())
    );
    assert_eq!(
        cold.mem.contents("/state/.state.json.tmp").as_deref(),
        Some(b"partial".as_slice())
    );
}
