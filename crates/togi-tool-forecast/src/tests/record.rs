use std::fs;

use togi_journal::Seq;
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use super::support::{A, events, run, start};
use crate::{Error, File, Record, Session, Status, recovery_bias};

fn record() -> Record {
    let anchor = Session::from_events(events(1, start(A, "abc1234")))
        .unwrap()
        .anchor_at(Seq::new(2).unwrap())
        .unwrap();
    let runs = vec![
        run(Status::Concluded, 2.0, 1, 0, &[-12, -20]),
        run(Status::DeadEnd, 6.5, 2, 1, &[-8, -24]),
        run(Status::Censored, 9.0, 3, 2, &[-10, -22]),
    ];
    let files = vec![File {
        path: "machines/m.json".to_owned(),
        sha256: "00".repeat(32),
    }];
    Record::new(anchor, "abc1234", false, files, runs).unwrap()
}

#[test]
fn a_new_record_carries_this_builds_ruleset_and_the_summary_of_its_runs() {
    let record = record();
    assert_eq!((record.ruleset, record.evidence), (RULESET, EVIDENCE_EPOCH));
    assert_eq!(record.summary, crate::summarize(&record.runs).unwrap());
    assert_eq!(
        (
            record.summary.concluded,
            record.summary.dead_ends,
            record.summary.censored
        ),
        (1, 1, 1)
    );
}

#[test]
fn a_record_without_runs_cannot_be_made() {
    let anchor = record().anchor;
    assert!(matches!(
        Record::new(anchor, "abc1234", false, Vec::new(), Vec::new()),
        Err(Error::NoRuns)
    ));
}

#[test]
fn a_record_written_reads_back_equal_and_is_the_same_bytes_every_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forecast.json");
    let record = record();
    let mut bytes = Vec::new();
    record.write(&mut bytes).unwrap();
    let mut again = Vec::new();
    record.write(&mut again).unwrap();
    assert_eq!(bytes, again);
    assert!(bytes.ends_with(b"}\n"));
    assert!(bytes.windows(3).any(|w| w == b"\n  "), "indented");
    fs::write(&path, &bytes).unwrap();
    assert_eq!(Record::read(&path).unwrap(), record);
}

#[test]
fn a_record_is_plain_json_with_named_statuses_and_cores() {
    let mut bytes = Vec::new();
    record().write(&mut bytes).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["commit"], "abc1234");
    assert_eq!(json["dirty"], false);
    assert_eq!(json["ruleset"], RULESET);
    assert_eq!(json["evidence"], EVIDENCE_EPOCH);
    assert_eq!(json["anchor"]["session"], A);
    assert_eq!(json["files"][0]["path"], "machines/m.json");
    let statuses: Vec<_> = json["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["outcome"]["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, ["concluded", "deadend", "censored"]);
    assert_eq!(json["runs"][0]["outcome"]["profile"]["1"], -20);
    assert_eq!(json["summary"]["dead_ends"], 1);
    assert_eq!(json["summary"]["cores"]["0"]["median"], -10.0);
    assert!(json["summary"]["hours"].is_object());
}

#[test]
fn a_summary_without_hours_writes_null() {
    let mut record = record();
    record
        .runs
        .retain(|run| run.outcome.status != Status::Concluded);
    record.summary = crate::summarize(&record.runs).unwrap();
    let mut bytes = Vec::new();
    record.write(&mut bytes).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json["summary"]["hours"].is_null());
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("f.json"), &bytes).unwrap();
    assert_eq!(
        Record::read(dir.path().join("f.json"))
            .unwrap()
            .summary
            .hours,
        None
    );
}

#[test]
fn a_record_that_cannot_be_read_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.json");
    let error = Record::read(&missing).unwrap_err();
    assert!(matches!(error, Error::ReadRecord { .. }));
    assert!(error.to_string().starts_with("read forecast "), "{error}");
    let broken = dir.path().join("broken.json");
    fs::write(&broken, b"{\"anchor\":").unwrap();
    let error = Record::read(&broken).unwrap_err();
    assert!(matches!(error, Error::DecodeRecord { .. }));
    assert!(error.to_string().contains("broken.json"), "{error}");
}

#[test]
fn a_record_without_runs_is_refused_on_reading() {
    let dir = tempfile::tempdir().unwrap();
    let mut json = serde_json::to_value(record()).unwrap();
    json["runs"] = serde_json::json!([]);
    let path = dir.path().join("empty.json");
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    let error = Record::read(&path).unwrap_err();
    assert!(matches!(error, Error::EmptyRecord { .. }));
    assert!(error.to_string().ends_with("has no runs"), "{error}");
}

#[test]
fn a_record_with_an_unknown_status_is_refused_on_reading() {
    let dir = tempfile::tempdir().unwrap();
    let mut json = serde_json::to_value(record()).unwrap();
    json["runs"][0]["outcome"]["status"] = serde_json::json!("running");
    let path = dir.path().join("status.json");
    fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(matches!(
        Record::read(&path),
        Err(Error::DecodeRecord { .. })
    ));
}

fn read_value(json: &serde_json::Value) -> Error {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("forecast.json");
    fs::write(&path, serde_json::to_vec(json).unwrap()).unwrap();
    Record::read(&path).unwrap_err()
}

fn positional(object: &serde_json::Value, keys: &[&str]) -> serde_json::Value {
    keys.iter().map(|key| object[*key].clone()).collect()
}

fn at_index<'a>(json: &'a mut serde_json::Value, path: &[&str]) -> &'a mut serde_json::Value {
    path.iter()
        .fold(json, |value, key| match key.parse::<usize>() {
            Ok(index) => &mut value[index],
            Err(_) => &mut value[*key],
        })
}

#[test]
fn a_correctly_sized_array_is_refused_for_every_object_of_a_record() {
    let whole = serde_json::to_value(record()).unwrap();
    let anchor = ["session", "seq", "time", "sha256"].as_slice();
    let run = ["machine", "seed", "split", "outcome"].as_slice();
    let outcome = ["status", "hours", "crashes", "hunts", "profile", "depth"].as_slice();
    let range = ["min", "p10", "median", "p90", "max"].as_slice();
    let summary = [
        "runs",
        "concluded",
        "dead_ends",
        "censored",
        "hours",
        "crashes",
        "hunts",
        "depth",
        "cores",
    ]
    .as_slice();
    let cases: [(&str, Vec<&str>, &[&str]); 7] = [
        (
            "record",
            vec![],
            &[
                "anchor", "commit", "dirty", "ruleset", "evidence", "files", "runs", "summary",
            ],
        ),
        ("anchor", vec!["anchor"], anchor),
        ("file", vec!["files", "0"], &["path", "sha256"]),
        ("run", vec!["runs", "0"], run),
        ("outcome", vec!["runs", "0", "outcome"], outcome),
        ("summary", vec!["summary"], summary),
        ("range", vec!["summary", "crashes"], range),
    ];
    for (name, path, keys) in cases {
        let mut json = whole.clone();
        let object = at_index(&mut json, &path);
        *object = positional(object, keys);
        let error = read_value(&json);
        assert!(
            matches!(error, Error::DecodeRecord { .. }),
            "{name}: {error}"
        );
        assert!(
            error.to_string().contains("expected an object"),
            "{name}: {error}"
        );
    }
}

#[test]
fn an_unknown_key_is_refused_in_every_object_of_a_record() {
    let whole = serde_json::to_value(record()).unwrap();
    let cases: [(&str, Vec<&str>); 7] = [
        ("record", vec![]),
        ("anchor", vec!["anchor"]),
        ("file", vec!["files", "0"]),
        ("run", vec!["runs", "0"]),
        ("outcome", vec!["runs", "0", "outcome"]),
        ("summary", vec!["summary"]),
        ("range", vec!["summary", "crashes"]),
    ];
    for (name, path) in cases {
        let mut json = whole.clone();
        at_index(&mut json, &path)["hourz"] = serde_json::json!(1);
        let error = read_value(&json);
        assert!(
            matches!(error, Error::DecodeRecord { .. }),
            "{name}: {error}"
        );
        assert!(
            error.to_string().contains("unknown field `hourz`"),
            "{name}: {error}"
        );
    }
}

struct Full;

impl std::io::Write for Full {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("full"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct FullAtNewline;

impl std::io::Write for FullAtNewline {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf == b"\n" {
            return Err(std::io::Error::other("full"));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn writing_to_a_failing_sink_is_an_error() {
    assert!(matches!(
        record().write(&mut Full),
        Err(Error::WriteRecord(_))
    ));
    assert!(matches!(
        record().write(&mut FullAtNewline),
        Err(Error::WriteRecord(_))
    ));
}

#[test]
fn the_recovery_bias_states_the_simulated_reboot_time() {
    let text = recovery_bias();
    assert!(
        text.contains("a simulated crash reboot takes 90 s"),
        "{text}"
    );
    assert!(text.ends_with("not corrected"), "{text}");
}
