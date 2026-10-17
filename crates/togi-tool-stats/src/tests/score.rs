use std::path::Path;

use togi_journal::{
    Build, DeadEnd, DeadEndCondition, Event, SCHEMA, SessionBaseline, ShutdownReason,
};
use togi_machine::{ObservedOffset, PerCore};
use togi_tool_forecast::{Anchor, Outcome, Record, Run, Session, Split, Status};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use super::support::{Log, config_loaded, crash_detected, shutdown};
use crate::Error;
use crate::forecast::{render_score, score_with};

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
        split: Split::Dev,
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
fn a_forecast_read_cause_reaches_the_stats_boundary_once() {
    struct Missing;
    impl std::io::Read for Missing {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("missing forecast"))
        }
    }
    let error = Record::decode(Missing, "missing.json").unwrap_err();
    assert_eq!(
        togi_machine::error_chain(&Error::from(error)),
        "read forecast missing.json: missing forecast"
    );
}

#[test]
fn a_forecast_decode_cause_reaches_the_stats_boundary_once() {
    let error = Record::decode(b"{\"anchor\":".as_slice(), "broken.json").unwrap_err();
    assert_eq!(
        togi_machine::error_chain(&Error::from(error)),
        "decode forecast broken.json: EOF while parsing a value at line 1 column 10"
    );
}

#[test]
fn a_concluded_run_is_scored_against_the_ensemble() {
    let mut log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    log.at(3 * 3600).add(config_loaded(stamped(Some("abc"))));
    log.at(4 * 3600).add(shutdown(ShutdownReason::Cycles));
    assert_snapshot!(
        "score__a_concluded_run_is_scored_against_the_ensemble",
        scored(&record, log.events)
    );
}

#[test]
fn a_run_that_has_not_concluded_is_scored_with_lower_bounds() {
    let mut log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    log.at(3600).add(config_loaded(stamped(Some("abc"))));
    log.at(2 * 3600).add(crash_detected("a", None));
    assert_snapshot!(
        "score__a_run_that_has_not_concluded_is_scored_with_lower_bounds",
        scored(&record, log.events)
    );
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
    assert_snapshot!(
        "score__a_forecast_without_a_concluded_run_has_no_hours_to_score_against",
        scored(&record, log.events)
    );
}

#[test]
fn flags_name_what_makes_the_forecast_less_comparable() {
    let mut log = opening(Some("old"));
    let record = record(&log, "new", true, ensemble(true));
    log.at(3600).add(config_loaded(stamped(None)));
    log.at(7200).add(shutdown(ShutdownReason::Cycles));
    assert_snapshot!(
        "score__flags_name_what_makes_the_forecast_less_comparable",
        scored(&record, log.events)
    );
}

#[test]
fn a_journal_with_nothing_after_the_anchor_scores_no_builds() {
    let log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    assert_snapshot!(
        "score__a_journal_with_nothing_after_the_anchor_scores_no_builds",
        scored(&record, log.events)
    );
}

fn encoded(log: &Log) -> Vec<u8> {
    log.events
        .iter()
        .map(|event| event.encode().unwrap() + "\n")
        .collect::<String>()
        .into_bytes()
}

fn score_memory(record: &Record, journal: &[u8]) -> Result<crate::Scored, Error> {
    let path = Path::new("forecast.json");
    let mut bytes = Vec::new();
    record.write(&mut bytes).unwrap();
    score_with(
        path,
        || Record::decode(bytes.as_slice(), path).map_err(Error::from),
        |_| {
            Ok(Session::decode(Path::new("events.jsonl"), journal)?
                .into_iter()
                .collect())
        },
    )
}

#[test]
fn a_state_directory_is_scored_from_the_anchor_through_the_forecast_file() {
    let mut log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    log.at(3600).add(config_loaded(stamped(Some("abc"))));
    log.at(7200).add(shutdown(ShutdownReason::Cycles));
    let text = score_memory(&record, &encoded(&log)).unwrap().text;
    assert!(text.contains(", unchanged\n"), "{text}");
    assert!(text.contains("\nForecast score\n"), "{text}");
    assert_eq!(text, scored(&record, log.events.clone()));
}

#[test]
fn a_state_directory_whose_journal_through_the_anchor_changed_is_refused() {
    let mut log = opening(Some("abc"));
    let record = record(&log, "abc", false, ensemble(true));
    log.at(3600).add(config_loaded(stamped(Some("abc"))));
    let changed =
        String::from_utf8(encoded(&log))
            .unwrap()
            .replacen("\"boot\":\"", "\"boot\":\"x", 1);
    let error = score_memory(&record, changed.as_bytes()).unwrap_err();
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
    assert!(
        togi_machine::error_chain(&error).contains("anchor changed"),
        "{error}"
    );
}

#[test]
fn a_missing_forecast_file_names_the_file() {
    let missing = Path::new("absent.json");
    let error = score_with(
        missing,
        || {
            Err(togi_tool_forecast::Error::ReadRecord {
                path: missing.to_path_buf(),
                source: std::io::ErrorKind::NotFound.into(),
            }
            .into())
        },
        |_| panic!("a missing forecast must fail before reading the journal"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("absent.json"), "{error}");
}
