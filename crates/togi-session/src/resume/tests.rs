use std::num::NonZeroU32;

use togi_defect::{DefectId, DefectState, Entry};
use togi_journal::{DefectDirection, Payload};
use togi_machine::Signal;
use togi_sim::MachineDefinition;
use togi_sim::definition::ModelDef;

use super::*;
use crate::publication::StateDocument;
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
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    input.resume = resume;
    run(input)
}

fn capsule_of(events: &[Event], defects: &'static [Entry]) -> ResumeCapsule {
    ResumeCapsule {
        session: None,
        last: last_seq(events),
        defects,
        folds: Folds::replay_with(defects, events).unwrap(),
    }
}

fn fold_of(events: &[Event]) -> Folds {
    Folds::replay_with(&[], events).unwrap()
}

fn halves(ended: &Ended) -> (&[Event], &[Event]) {
    let events = ended.journal.events();
    (&events[..events.len() / 2], events)
}

#[test]
fn a_capsule_verifies_the_state_it_folded() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    assert_eq!(ended.stop, Some(Stop::Cycles));
    let capsule = ended.capsule.as_ref().unwrap();
    capsule.verify(ended.journal.events()).unwrap();
}

#[test]
fn verify_names_the_part_that_differs() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    let (prefix, events) = halves(&ended);
    let behind = fold_of(prefix);

    let mut facts = capsule_of(events, &[]);
    facts.folds.facts = behind.facts.clone();
    assert!(matches!(facts.verify(events), Err(Divergence::Facts)));

    let mut tuner = capsule_of(events, &[]);
    tuner.folds.tuner = behind.tuner.clone();
    assert!(matches!(tuner.verify(events), Err(Divergence::Tuner)));

    let mut recovery = capsule_of(events, &[]);
    recovery.folds.recovery = behind.recovery;
    assert!(matches!(recovery.verify(events), Err(Divergence::Recovery)));

    let mut defects = capsule_of(events, &CAUTIOUS);
    defects.folds.defects = DefectState::new(&AGGRESSIVE);
    assert!(matches!(defects.verify(events), Err(Divergence::Defects)));
}

#[test]
fn a_verified_capsules_document_round_trips_through_json() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    let capsule = ended.capsule.as_ref().unwrap();
    capsule.verify(ended.journal.events()).unwrap();
    let document = StateDocument::of(&capsule.folds);
    let decoded: StateDocument =
        serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
    assert_eq!(decoded, document);
    assert!(!document.tuner.cores.is_empty());
    check_document(&document, &document).unwrap();
}

#[test]
fn the_document_check_rejects_a_document_other_than_the_cold_folds() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    let (prefix, events) = halves(&ended);
    let document = StateDocument::of(&fold_of(events));
    let behind = StateDocument::of(&fold_of(prefix));
    assert_ne!(document, behind);
    assert!(matches!(
        check_document(&behind, &document),
        Err(DocumentDivergence::Cold)
    ));
    assert!(matches!(
        check_document(&document, &behind),
        Err(DocumentDivergence::Cold)
    ));
}

#[test]
fn verify_rejects_a_capsule_that_misses_events() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    let (prefix, events) = halves(&ended);
    let capsule = capsule_of(prefix, &[]);
    assert!(matches!(
        capsule.verify(events),
        Err(Divergence::Last { .. })
    ));
}

#[test]
fn a_capsule_is_adopted_only_when_it_covers_exactly_the_journal() {
    let bench = bench();
    let ended = one_cycle(&bench, None);
    let capsule = ended.capsule.clone().unwrap();
    assert!(capsule.clone().adopt(&ended.journal, &[]).is_some());

    let foreign = ResumeCapsule {
        session: None,
        ..capsule.clone()
    };
    assert!(
        foreign.adopt(&ended.journal, &[]).is_none(),
        "another session"
    );

    drop(ended);
    let extended = one_cycle(&bench, None);
    assert!(
        capsule.adopt(&extended.journal, &[]).is_none(),
        "a journal that grew"
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
fn a_run_given_a_capsule_appends_what_a_cold_replay_would() {
    let warm = bench();
    let first = one_cycle(&warm, None);
    let capsule = first.capsule.clone().unwrap();
    drop(first);
    let resumed = one_cycle(&warm, Some(capsule));

    let cold = bench();
    drop(one_cycle(&cold, None));
    let replayed = one_cycle(&cold, None);

    assert_eq!(resumed.stop, replayed.stop);
    assert_eq!(journal_text(&resumed), journal_text(&replayed));
    resumed
        .capsule
        .unwrap()
        .verify(resumed.journal.events())
        .unwrap();
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
    crashed_publishing(bench, crate::PublicationPolicy::OnFinish)
}

fn crashed_publishing(bench: &Bench, publication: crate::PublicationPolicy) -> Ended {
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
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    input.publication = publication;
    let ended = run(input);
    assert!(
        ended.errors.iter().any(crate::SessionError::is_crash),
        "{:?}",
        ended.errors
    );
    ended
}

#[test]
fn a_crash_leaves_the_state_unwritten_and_the_capsule_holding_what_the_boot_folded() {
    let bench = bench();
    let ended = crashed(&bench);
    ended
        .capsule
        .as_ref()
        .unwrap()
        .verify(ended.journal.events())
        .unwrap();
    let saved: Option<crate::publication::StateDocument> = ended.journal.read_state().unwrap();
    assert!(saved.is_none());
}

#[test]
fn the_boot_after_a_crash_resumes_the_same_from_a_capsule_or_a_replay() {
    let warm = bench();
    let capsule = crashed(&warm).capsule.unwrap();
    warm.sim.reboot();
    let resumed = one_cycle(&warm, Some(capsule));

    let cold = bench();
    drop(crashed(&cold));
    cold.sim.reboot();
    let replayed = one_cycle(&cold, None);

    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    assert_eq!(resumed.stop, replayed.stop);
    assert_eq!(journal_text(&resumed), journal_text(&replayed));
    let saved: crate::publication::StateDocument = resumed.journal.read_state().unwrap().unwrap();
    let facts = togi_journal::State::replay(resumed.journal.events()).unwrap();
    assert_eq!(saved.facts, facts);
}

#[test]
fn a_crashed_runs_capsule_publishes_what_a_live_run_had_and_the_next_boot_rebuilds_nothing() {
    let deferred = bench();
    let mut crashed = crashed(&deferred);
    let capsule = crashed.capsule.take().unwrap();
    capsule.publish(&mut crashed.journal).unwrap();
    let published: StateDocument = crashed.journal.read_state().unwrap().unwrap();

    let live = bench();
    let live_crashed = crashed_publishing(&live, crate::PublicationPolicy::Live);
    let had: StateDocument = live_crashed.journal.read_state().unwrap().unwrap();
    assert_eq!(published, had);
    assert_eq!(journal_text(&crashed), journal_text(&live_crashed));
    drop(crashed);

    deferred.sim.reboot();
    let resumed = one_cycle(&deferred, None);
    assert!(
        resumed
            .journal
            .events()
            .iter()
            .all(|event| !matches!(event.payload, Payload::StateRebuilt(_)))
    );
}
