use std::fs;

use togi_journal::{
    Build, DeadEnd, DeadEndCondition, Event, SCHEMA, SessionBaseline, ShutdownReason,
};
use togi_machine::{ObservedOffset, PerCore};
use togi_tool_forecast::{Anchor, Outcome, Record, Run, Session, Status};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use super::support::{Log, config_loaded, crash_detected, shutdown};
use crate::forecast::render_score;
use crate::{Error, score};

fn stamped(rev: Option<&str>) -> Build {
    Build {
        version: "1.0.0".to_owned(),
        rev: rev.map(str::to_owned),
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: 0,
        evidence: EVIDENCE_EPOCH,
    }
}

fn baseline(offsets: [i64; 2]) -> SessionBaseline {
    SessionBaseline {
        offsets: offsets
            .iter()
            .enumerate()
            .map(|(n, &offset)| {
                (
                    super::support::core(i64::try_from(n).unwrap()),
                    ObservedOffset(offset),
                )
            })
            .collect::<PerCore<_>>(),
    }
}

/// A session of two cores with its baseline, ready to be anchored at its second event.
fn opening(rev: Option<&str>) -> Log {
    let mut log = Log::with_build(&[(0, 0), (1, 1)], stamped(rev));
    log.add(baseline([-10, -20]));
    log
}

fn run(status: Status, hours: f64, crashes: u32, hunts: u32, profile: [i64; 2]) -> Run {
    Run {
        machine: "m.json".to_owned(),
        seed: 0,
        split: "dev".to_owned(),
        outcome: Outcome {
            status,
            hours,
            crashes,
            hunts,
            profile: profile
                .iter()
                .enumerate()
                .map(|(n, &offset)| {
                    (
                        super::support::core(i64::try_from(n).unwrap()),
                        ObservedOffset(offset),
                    )
                })
                .collect(),
            depth: profile.iter().sum(),
        },
    }
}

fn ensemble(concluded: bool) -> Vec<Run> {
    let first = if concluded {
        Status::Concluded
    } else {
        Status::DeadEnd
    };
    vec![
        run(first, 2.0, 0, 0, [-12, -20]),
        run(first, 4.0, 1, 1, [-10, -22]),
        run(Status::DeadEnd, 6.0, 2, 1, [-8, -24]),
    ]
}

fn anchor(log: &Log) -> Anchor {
    Session::from_events(log.events.clone())
        .unwrap()
        .anchor_at(super::support::seq(2))
        .unwrap()
}

fn record(log: &Log, commit: &str, dirty: bool, runs: Vec<Run>) -> Record {
    Record::new(anchor(log), commit, dirty, Vec::new(), runs).unwrap()
}

fn scored(record: &Record, events: Vec<Event>) -> String {
    let session = Session::from_events(events).unwrap();
    render_score(&record.score(&[session]).unwrap())
}

#[test]
fn a_concluded_run_is_scored_against_the_ensemble() {
    let mut log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    log.at(3 * 3600).add(config_loaded(stamped(Some("abc"))));
    log.at(4 * 3600).add(shutdown(ShutdownReason::Cycles));
    insta::assert_snapshot!(scored(&record, log.events));
}

#[test]
fn a_run_that_has_not_concluded_is_scored_with_lower_bounds() {
    let mut log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    log.at(3600).add(config_loaded(stamped(Some("abc"))));
    log.at(2 * 3600).add(crash_detected("a", None));
    insta::assert_snapshot!(scored(&record, log.events));
}

#[test]
fn a_forecast_without_a_concluded_run_has_no_hours_to_score_against() {
    let mut log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(false));
    log.at(3600).add(config_loaded(stamped(Some("abc"))));
    log.at(2 * 3600).add(DeadEnd {
        condition: DeadEndCondition::FailureAtZero,
        core: None,
        detail: "test".to_owned(),
        action: None,
    });
    insta::assert_snapshot!(scored(&record, log.events));
}

#[test]
fn flags_name_what_makes_the_forecast_less_comparable() {
    let mut log = opening(Some("old"));
    let record = record(&log, "new", true, ensemble(true));
    log.at(3600).add(config_loaded(stamped(None)));
    log.at(7200).add(shutdown(ShutdownReason::Cycles));
    insta::assert_snapshot!(scored(&record, log.events));
}

#[test]
fn a_journal_with_nothing_after_the_anchor_scores_no_builds() {
    let log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    insta::assert_snapshot!(scored(&record, log.events));
}

fn state_dir(log: &Log) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut text = String::new();
    for event in &log.events {
        text.push_str(&event.encode().unwrap());
        text.push('\n');
    }
    fs::write(dir.path().join("events.jsonl"), text).unwrap();
    dir
}

fn write(record: &Record, dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("forecast.json");
    let mut bytes = Vec::new();
    record.write(&mut bytes).unwrap();
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn a_state_directory_is_scored_from_the_anchor_through_the_forecast_file() {
    let mut log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    log.at(3600).add(config_loaded(stamped(Some("abc"))));
    log.at(7200).add(shutdown(ShutdownReason::Cycles));
    let state = state_dir(&log);
    let forecast = write(&record, state.path());
    let text = score(state.path(), &forecast).unwrap().text;
    assert!(text.contains(", unchanged\n"), "{text}");
    assert!(text.contains("\nForecast score\n"), "{text}");
    assert_eq!(text, scored(&record, log.events.clone()));
}

#[test]
fn a_state_directory_whose_journal_through_the_anchor_changed_is_refused() {
    let mut log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    log.at(3600).add(config_loaded(stamped(Some("abc"))));
    let state = state_dir(&log);
    let forecast = write(&record, state.path());
    let journal = state.path().join("events.jsonl");
    let changed = fs::read_to_string(&journal)
        .unwrap()
        .replacen("\"boot\":\"", "\"boot\":\"x", 1);
    fs::write(&journal, changed).unwrap();
    let error = score(state.path(), &forecast).unwrap_err();
    assert!(
        matches!(
            &error,
            Error::Score {
                source: togi_tool_forecast::Error::ChangedAnchor { .. },
                ..
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("anchor changed"), "{error}");
}

#[test]
fn a_missing_forecast_file_names_the_file() {
    let state = state_dir(&opening(None));
    let missing = state.path().join("absent.json");
    let error = score(state.path(), &missing).unwrap_err();
    assert!(error.to_string().contains("absent.json"), "{error}");
}
