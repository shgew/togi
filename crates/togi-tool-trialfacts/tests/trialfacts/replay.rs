use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use togi_facts::{Decisive, TrialClass};
use togi_machine::{Regime, Signal, TargetOffset};
use togi_sim::definition::BiosContextDef;
use togi_sim::replay::{ReplayFact, ReplayOutcome};
use togi_sim::{Load, MachineDefinition};
use togi_tool_trialfacts::{
    Class, Extracts, ExtractsError, ReadError, TrialFact, replay_facts, write,
};

use crate::builders::{bios, crash, fact, offsets, set, workload};

const FIRST: &str = "20260101T000000Z";

fn machine(facts: &str) -> MachineDefinition {
    MachineDefinition {
        facts: Some(facts.to_owned()),
        bios_context: Some(BiosContextDef {
            bios_version: Some("fixture".into()),
            ..BiosContextDef::default()
        }),
        ..MachineDefinition::default()
    }
}

fn evidence() -> Vec<TrialFact> {
    let mut idle = fact(FIRST, 5, &[-20, 0], crash(None));
    idle.class = Class::Idle {
        loaded: set(&[0, 1]),
    };
    idle.condition = togi_machine::Condition::Together;
    let mut contextless = fact(FIRST, 6, &[-30, 0], Decisive::Pass);
    contextless.context = None;
    let mut elsewhere = fact(FIRST, 7, &[-40, 0], Decisive::Pass);
    elsewhere.context = Some(bios("other"));
    vec![
        fact(FIRST, 4, &[-10, 0], Decisive::Pass),
        idle,
        contextless,
        elsewhere,
    ]
}

fn answered(replay: &togi_sim::replay::Replay, context: &str, first_offset: i64) -> bool {
    let load = Load {
        regime: Regime::R1,
        workload: "mprime-sse-4k-21k",
        cores: set(&[0]),
        duration: Duration::from_secs(90),
    };
    replay.has_answer(&bios(context), &load, &offsets(&[first_offset, 0]))
}

fn extract_in(dir: &Path) -> PathBuf {
    let path = dir.join("facts.gz");
    write(&path, &evidence()).unwrap();
    path
}

#[test]
fn only_decisive_trials_with_a_context_answer_trials() {
    let dir = tempfile::tempdir().unwrap();
    let path = extract_in(dir.path());
    let machine_path = dir.path().join("machine.json");
    for facts in ["facts.gz", path.to_str().unwrap()] {
        let replay = Extracts::new()
            .replay(&machine_path, &machine(facts))
            .unwrap();
        assert_eq!(replay.len(), 2, "{facts}");
        for first_offset in [-10, -20, -30, -40] {
            assert_eq!(
                answered(&replay, "fixture", first_offset),
                first_offset == -10,
                "{facts} {first_offset}"
            );
        }
        assert!(answered(&replay, "other", -40));
        assert!(!answered(&replay, "other", -10));
    }
}

#[test]
fn replay_facts_keep_what_the_simulator_matches_and_replays() {
    let mut failure = fact(
        FIRST,
        8,
        &[-45, 0],
        Decisive::Failure {
            signal: Signal::UncorrectedMce,
            core: None,
            stalled: None,
        },
    );
    failure.duration_s = 0;
    failure.class = Class::Trial(TrialClass {
        workload: workload(Regime::R2, 0),
        loaded: set(&[0, 1]),
        duration_s: 120,
    });
    let mut facts = evidence();
    facts.push(failure);
    let replayed = replay_facts(&facts);
    assert_eq!(
        replayed,
        [
            ReplayFact {
                context: bios("fixture"),
                regime: Regime::R1,
                workload: "mprime-sse-4k-21k".into(),
                cores: set(&[0]),
                duration: Duration::from_secs(90),
                profile: offsets(&[-10, 0]),
                outcome: ReplayOutcome::Pass,
            },
            ReplayFact {
                context: bios("other"),
                regime: Regime::R1,
                workload: "mprime-sse-4k-21k".into(),
                cores: set(&[0]),
                duration: Duration::from_secs(90),
                profile: offsets(&[-40, 0]),
                outcome: ReplayOutcome::Pass,
            },
            ReplayFact {
                context: bios("fixture"),
                regime: Regime::R2,
                workload: facts[4].class.workload().unwrap().as_str().into(),
                cores: set(&[0, 1]),
                duration: Duration::from_secs(120),
                profile: vec![TargetOffset::new(-45).unwrap(), TargetOffset::ZERO],
                outcome: ReplayOutcome::Failure {
                    signal: Signal::UncorrectedMce,
                    exposure: Duration::ZERO,
                },
            },
        ]
    );
}

#[test]
fn a_machine_without_an_extract_or_context_or_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    extract_in(dir.path());
    let machine_path = dir.path().join("machine.json");
    let mut extracts = Extracts::new();

    for facts in [None, Some(String::new())] {
        let declared = MachineDefinition {
            facts,
            ..machine("facts.gz")
        };
        let error = extracts.replay(&machine_path, &declared).unwrap_err();
        assert!(matches!(error, ExtractsError::NoFacts), "{error}");
    }

    let undeclared = MachineDefinition {
        bios_context: None,
        ..machine("facts.gz")
    };
    let error = extracts.replay(&machine_path, &undeclared).unwrap_err();
    assert!(matches!(error, ExtractsError::NoContext), "{error}");

    let error = extracts
        .replay(&machine_path, &machine("missing.gz"))
        .unwrap_err();
    assert!(
        matches!(error, ExtractsError::Read(ReadError::Open { .. })),
        "{error}"
    );
}

#[test]
fn a_fact_the_simulator_cannot_replay_names_its_index() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("facts.gz");
    let mut too_long = fact(FIRST, 5, &[-10, 0], crash(Some(0)));
    too_long.duration_s = 91;
    write(
        &path,
        &[fact(FIRST, 4, &[-10, 0], Decisive::Pass), too_long],
    )
    .unwrap();
    let error = Extracts::new()
        .replay(&dir.path().join("machine.json"), &machine("facts.gz"))
        .unwrap_err();
    assert!(
        matches!(&error, ExtractsError::Invalid(invalid) if invalid.index == 1),
        "{error}"
    );
}

#[test]
fn each_extract_is_read_once_and_shared() {
    let dir = tempfile::tempdir().unwrap();
    let path = extract_in(dir.path());
    let mut extracts = Extracts::default();
    let mut first_records = None;
    for machine_name in ["first.json", "second.json"] {
        let (found, records) = extracts
            .load(&dir.path().join(machine_name), &machine("facts.gz"))
            .unwrap();
        assert_eq!(found, path);
        assert_eq!(records.len(), 4);
        match &first_records {
            None => first_records = Some(records),
            Some(first) => assert!(Arc::ptr_eq(first, &records)),
        }
        std::fs::remove_file(&path).ok();
    }
    assert!(
        Extracts::new()
            .load(&dir.path().join("first.json"), &machine("facts.gz"))
            .is_err()
    );
}

#[test]
fn extracts_resolve_relative_to_the_machine_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("machines")).unwrap();
    std::fs::create_dir(dir.path().join("facts")).unwrap();
    write(dir.path().join("facts").join("e.gz"), &evidence()).unwrap();
    let (path, records) = Extracts::new()
        .load(
            &dir.path().join("machines").join("m.json"),
            &machine("../facts/e.gz"),
        )
        .unwrap();
    assert_eq!(path, dir.path().join("machines").join("../facts/e.gz"));
    assert_eq!(records.len(), 4);
}
