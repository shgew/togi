use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::GzDecoder;
use togi_facts::{Decisive, TrialClass};
use togi_journal::{Activity, Dimension, Failure, MemFs, Outcome, Payload, SCHEMA};
use togi_machine::{CcdId, Condition, PerCore, Regime, Signal};
use togi_tool_trialfacts::{
    Class, ExtractError, InvalidProfile, Phase, TrialFact, decode, encode, extract, from_recordings,
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

    let facts = extract(&dir).unwrap().facts;
    assert_eq!(facts, [passed(OLD, 1), passed(NEW, 6)]);

    let first = encoded(&facts);
    assert_eq!(first, encoded(&extract(&dir).unwrap().facts));
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
    let facts = extract(&w.close()).unwrap().facts;
    let sessions: Vec<_> = facts.iter().map(|fact| fact.session.to_string()).collect();
    assert_eq!(sessions, [OLD, NEW]);
}

#[test]
fn recordings_extract_like_their_state_directory() {
    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    passing_session(&mut w, OLD, 1);
    let mut w = w.archive();
    passing_session(&mut w, NEW, 1);
    let dir = w.close();
    let recordings = dir.recordings_lenient().unwrap();
    assert_eq!(
        from_recordings(recordings).unwrap().facts,
        extract(&dir).unwrap().facts
    );
}

/// Inserts an event of a kind no build knows after the event `after` of the live journal and renumbers the lines
/// behind it, so the journal is whole but for what a strict read would refuse.
fn splice_unknown_event(fs: &MemFs, after: u64) {
    let path = "/state/events.jsonl";
    let text = String::from_utf8(fs.contents(path).unwrap()).unwrap();
    let mut lines = Vec::new();
    for line in text.lines() {
        let mut line: serde_json::Value = serde_json::from_str(line).unwrap();
        let seq = line["seq"].as_u64().unwrap();
        if seq > after {
            line["seq"] = (seq + 1).into();
        }
        if seq == after {
            let mut unknown = line.clone();
            unknown["seq"] = (seq + 1).into();
            unknown["kind"] = "future.thing".into();
            unknown["data"] = serde_json::json!({});
            lines.push(line);
            lines.push(unknown);
        } else {
            lines.push(line);
        }
    }
    let spliced: String = lines.iter().map(|line| line.to_string() + "\n").collect();
    fs.put(path, spliced);
}

#[test]
fn an_event_of_an_unknown_kind_in_the_middle_is_skipped_and_reported() {
    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    w.begin(OLD, 1, &[0, 1]);
    w.trial(alone("1", 0, -10, &[-10, 0]), end("1", Outcome::Pass, 90));
    w.trial(alone("2", 0, -10, &[-10, 0]), end("2", Outcome::Pass, 90));
    drop(w.close());
    let whole = extract(&lock(&fs)).unwrap();
    assert!(whole.skipped.is_empty());

    splice_unknown_event(&fs, 4);
    let extracted = extract(&lock(&fs)).unwrap();

    let mut want = whole.facts;
    want[1].seq = want[1].seq.next().unwrap();
    assert_eq!(extracted.facts, want);
    let [(session, skipped)] = extracted.skipped.as_slice() else {
        panic!("expected one session with skips: {:?}", extracted.skipped);
    };
    assert_eq!(session.to_string(), OLD);
    let [event] = skipped.events() else {
        panic!("expected one skipped event: {skipped}");
    };
    assert_eq!((event.seq.get(), event.kind.as_str()), (5, "future.thing"));
    assert!(skipped.to_string().contains("future.thing"), "{skipped}");
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

    let facts = extract(&w.close()).unwrap().facts;
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
    let facts = extract(&w.close()).unwrap().facts;
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
    assert_eq!(
        error.to_string(),
        "/state/events.jsonl: first line is not a complete header of this build; either another build's journal or a header torn by a crash; see docs/howto.md#go-to-rust-cutover"
    );

    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    w.begin(OLD, 1, &[0, 1]);
    w.add(alone("1", 0, -10, &[-10, 0]));
    w.add(alone("1", 0, -10, &[-10, 0]));
    let error = extract(&w.close()).unwrap_err();
    assert!(matches!(error, ExtractError::Facts { .. }), "{error}");
}

#[test]
fn a_lock_error_keeps_the_path_and_original_source() {
    use togi_journal::fs::{Fault, Op};
    let fs = MemFs::new();
    fs.inject(Fault::before(Op::Open, 1).named("lock"));
    #[expect(
        clippy::disallowed_methods,
        reason = "the fixture injects a lock failure in memory"
    )]
    let source = togi_journal::StateDir::simulated(fs).unwrap_err();
    assert_eq!(
        ExtractError::Lock(source).to_string(),
        "lock the state directory: lock /state/lock: injected Open fault"
    );
}

#[test]
fn a_session_whose_schema_is_not_readable_history_is_refused_by_name() {
    for schema in [0, SCHEMA + 1] {
        let fs = MemFs::new();
        let mut w = Writer::open(&fs);
        passing_session(&mut w, OLD, 1);
        let mut w = w.archive();
        w.begin_with(NEW, &[0, 1], |build| build.schema = schema);
        w.trial(alone("1", 0, -10, &[-10, 0]), end("1", Outcome::Pass, 90));
        let error = extract(&w.close()).unwrap_err();
        match &error {
            ExtractError::Incompatible { session, source } => {
                assert_eq!(session.to_string(), NEW);
                assert_eq!(source.dimension, Dimension::Schema);
                assert_eq!(source.recorded, schema);
                assert_eq!(source.current, SCHEMA);
            }
            other => panic!("schema {schema}: {other}"),
        }
        assert!(error.to_string().contains(NEW), "{error}");
    }
}

#[test]
fn a_different_ruleset_or_evidence_epoch_is_still_history() {
    let fs = MemFs::new();
    let mut w = Writer::open(&fs);
    w.begin_with(OLD, &[0, 1], |build| {
        build.ruleset = 9;
        build.evidence = 7;
    });
    w.trial(alone("1", 0, -10, &[-10, 0]), end("1", Outcome::Pass, 90));
    let mut expected = passed(OLD, 9);
    expected.build.evidence = Some(7);
    assert_eq!(extract(&w.close()).unwrap().facts, [expected]);
}

#[test]
fn hunt_and_deepening_activities_extract_as_their_phases() {
    for (activity, phase, name) in [
        (Activity::Hunt, Phase::Hunt, "hunt"),
        (Activity::Deepening, Phase::Deepening, "deepening"),
    ] {
        let fs = MemFs::new();
        let mut w = Writer::open(&fs);
        w.begin(OLD, 1, &[0, 1]);
        let mut intent = alone("1", 0, -10, &[-10, 0]);
        intent.activity = Some(activity);
        w.trial(intent, end("1", Outcome::Pass, 90));
        let facts = extract(&w.close()).unwrap().facts;
        assert_eq!(facts.len(), 1, "{name}");
        assert_eq!(facts[0].phase, Some(phase), "{name}");
        let plain = gunzip(&encoded(&facts));
        let record: serde_json::Value = serde_json::from_str(plain.trim()).unwrap();
        assert_eq!(record["phase"], name);
    }
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
