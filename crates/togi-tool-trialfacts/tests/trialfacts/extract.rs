use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::GzDecoder;
use togi_facts::{Decisive, TrialClass};
use togi_journal::{Failure, MemFs, Outcome, Payload};
use togi_machine::{CcdId, Condition, PerCore, Regime, Signal};
use togi_tool_trialfacts::{
    Class, ExtractError, InvalidProfile, Phase, TrialFact, decode, encode, extract, extract_dir,
};

use crate::builders::{
    BOOT, Writer, alone, core, crash, end, fact, failed, lock, per_core, set, stamp_of, together,
    workload,
};

const OLD: &str = "20260101T000000Z";
const NEW: &str = "20260102T000000Z";

const WHITELIST: [&str; 19] = [
    "session",
    "build",
    "ruleset",
    "context",
    "seq",
    "trial",
    "kind",
    "class",
    "condition",
    "phase",
    "record_only",
    "profile",
    "outcome",
    "signal",
    "duration_s",
    "stalled_core",
    "core",
    "voltage_requests_v",
    "top_requesters",
];

fn gunzip(bytes: &[u8]) -> String {
    let mut text = String::new();
    GzDecoder::new(bytes).read_to_string(&mut text).unwrap();
    text
}

fn encoded(facts: &[TrialFact]) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode(&mut bytes, facts).unwrap();
    bytes
}

fn passing_session(w: &mut Writer, session: &str, ruleset: u32) {
    w.begin(session, ruleset, &[0, 1]);
    w.trial(alone("1", 0, -10, &[-10, 0]), end("1", Outcome::Pass, 90));
}

fn passed(session: &str, ruleset: u32) -> TrialFact {
    let mut expected = fact(session, 4, &[-10, 0], Decisive::Pass);
    expected.build = stamp_of(ruleset);
    expected
}

#[test]
fn every_session_extracts_oldest_first_and_only_the_whitelist_survives() {
    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    passing_session(&mut w, OLD, 1);
    let mut w = w.archive();
    passing_session(&mut w, NEW, 6);
    let dir = w.close();

    let facts = extract(&dir).unwrap();
    assert_eq!(facts, [passed(OLD, 1), passed(NEW, 6)]);

    let first = encoded(&facts);
    assert_eq!(first, encoded(&extract(&dir).unwrap()));
    let plain = gunzip(&first);
    for private in [
        BOOT,
        "2023-11-14",
        "/state",
        "\"msg\"",
        "\"boot\"",
        "\"time\"",
    ] {
        assert!(!plain.contains(private), "{private} leaked into {plain}");
    }
    for line in plain.lines() {
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        for key in record.as_object().unwrap().keys() {
            assert!(
                WHITELIST.contains(&key.as_str()),
                "{key} is not whitelisted"
            );
        }
    }
}

#[test]
fn facts_follow_session_order_not_archive_order() {
    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    passing_session(&mut w, NEW, 1);
    let mut w = w.archive();
    passing_session(&mut w, OLD, 1);
    let facts = extract(&w.close()).unwrap();
    let sessions: Vec<_> = facts.iter().map(|fact| fact.session.to_string()).collect();
    assert_eq!(sessions, [OLD, NEW]);
}

#[test]
fn a_state_directory_on_disk_extracts_like_its_memory_image() {
    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    passing_session(&mut w, OLD, 1);
    let mut w = w.archive();
    passing_session(&mut w, NEW, 1);
    let dir = w.close();
    let want = extract(&dir).unwrap();
    drop(dir);

    let disk = tempfile::tempdir().unwrap();
    fs.dump_to(disk.path()).unwrap();
    assert_eq!(extract_dir(disk.path().join("state")).unwrap(), want);
}

#[test]
fn partial_r7_facts_keep_their_class_and_measurements() {
    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    w.begin(OLD, 8, &[0, 1]);
    let r7 = |id: &str, cores: &[i64]| {
        let mut intent = together(id, Regime::R7, cores, &[-30, -50], Condition::Together);
        intent.duration_s = 120;
        intent.activity = Some(togi_journal::Activity::Checking);
        intent
    };
    w.trial(r7("pass", &[0]), end("pass", Outcome::Pass, 120));
    let mut failure = failed("failure", Signal::Crash, 7);
    failure.core = Some(core(0));
    failure.stalled_core = Some(core(0));
    failure.voltage_requests_v = [(core(0), 1.125)].into_iter().collect();
    failure.top_requesters = set(&[0]);
    failure.ccd_mhz = BTreeMap::from([(CcdId(0), 4800)]);
    w.trial(r7("failure", &[0]), failure);
    w.trial(r7("full", &[0, 1]), end("full", Outcome::Pass, 120));

    let facts = extract(&w.close()).unwrap();
    let class = |cores: &[i64]| {
        Class::Trial(TrialClass {
            workload: workload(Regime::R7, 0),
            loaded: set(cores),
            duration_s: 120,
        })
    };
    let r7_fact = |seq: u64, trial: &str, cores: &[i64], outcome, duration_s| TrialFact {
        build: stamp_of(8),
        trial: Some(togi_journal::TrialId::from(trial.to_owned())),
        class: class(cores),
        condition: Condition::Together,
        phase: Some(Phase::Checking),
        duration_s,
        ..fact(OLD, seq, &[-30, -50], outcome)
    };
    let mut crashed = r7_fact(
        6,
        "failure",
        &[0],
        Decisive::Failure {
            signal: Signal::Crash,
            core: Some(core(0)),
            stalled: Some(core(0)),
        },
        7,
    );
    crashed.voltage_requests_v = [(core(0), 1.125)].into_iter().collect();
    crashed.top_requesters = set(&[0]);
    crashed.ccd_mhz = BTreeMap::from([(CcdId(0), 4800)]);
    assert_eq!(
        facts,
        [
            r7_fact(4, "pass", &[0], Decisive::Pass, 120),
            crashed,
            r7_fact(8, "full", &[0, 1], Decisive::Pass, 120),
        ]
    );
    let plain = gunzip(&encoded(&facts));
    assert_eq!(plain.matches("\"record_only\"").count(), 0);
    assert_eq!(decode(encoded(&facts).as_slice()).unwrap(), facts);
}

#[test]
fn a_failure_outside_any_trial_is_an_idle_fact_of_the_session_cores() {
    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    w.begin(OLD, 1, &[0, 1]);
    let seq = w.add(Payload::Failure(Failure {
        signal: Signal::Crash,
        attribution: togi_journal::Attribution::Unattributed,
        core: None,
        offset: None,
        trial: None,
        regime: None,
        condition: Some(Condition::Together),
        profile: Some(per_core(&[-10, -20])),
        known_failure: None,
        reason: Some("private reason".into()),
        round: None,
    }));
    let facts = extract(&w.close()).unwrap();
    let idle = TrialFact {
        class: Class::Idle {
            loaded: set(&[0, 1]),
        },
        condition: Condition::Together,
        phase: None,
        trial: None,
        duration_s: 0,
        ..fact(OLD, seq.get(), &[-10, -20], crash(None))
    };
    assert_eq!(facts, [idle]);
    let plain = gunzip(&encoded(&facts));
    assert!(!plain.contains("private reason"));
    assert_eq!(decode(encoded(&facts).as_slice()).unwrap(), facts);
}

#[test]
fn a_profile_with_a_hole_is_refused_with_its_session_and_event() {
    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    w.begin(OLD, 1, &[0, 2]);
    let mut intent = alone("1", 0, -10, &[]);
    intent.profile = [(0, -10), (2, 0)]
        .into_iter()
        .map(|(n, v)| (core(n), togi_machine::TargetOffset::new(v).unwrap()))
        .collect::<PerCore<_>>();
    w.trial(intent, end("1", Outcome::Pass, 90));
    let error = extract(&w.close()).unwrap_err();
    assert!(
        matches!(
            error,
            ExtractError::Profile {
                source: InvalidProfile::Sparse(1),
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn a_bad_source_is_an_error_and_no_facts() {
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", "{invalid}\n");
    let error = extract(&lock(&fs)).unwrap_err();
    assert!(matches!(error, ExtractError::Journals(_)), "{error}");

    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    w.begin(OLD, 1, &[0, 1]);
    w.add(alone("1", 0, -10, &[-10, 0]));
    w.add(alone("1", 0, -10, &[-10, 0]));
    let error = extract(&w.close()).unwrap_err();
    assert!(matches!(error, ExtractError::Facts { .. }), "{error}");
}

#[test]
fn no_evidence_is_refused() {
    let none = MemFs::new();
    let empty = MemFs::new();
    empty.put("/state/events.jsonl", "");
    let started = MemFs::new();
    let mut w = Writer::open(&started);
    w.begin(OLD, 1, &[0, 1]);
    w.close();
    for (name, fs) in [
        ("no journals", none),
        ("empty journal", empty),
        ("no facts", started),
    ] {
        let error = extract(&lock(&fs)).unwrap_err();
        assert!(matches!(error, ExtractError::NoFacts), "{name}: {error}");
    }
}
