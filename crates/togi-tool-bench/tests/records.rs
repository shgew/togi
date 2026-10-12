#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

mod common;

use common::record;
use togi_tool_bench::{Error, Record, Status, read_records, write_records};

fn read_text(text: &str) -> Result<Vec<Record>, Error> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records.jsonl");
    std::fs::write(&path, text).unwrap();
    read_records(&path, "baseline")
}

fn line(record: &Record) -> String {
    serde_json::to_string(record).unwrap()
}

#[test]
fn records_round_trip_through_a_file_one_per_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.jsonl");
    let mut with_r7 = record("shared", 2);
    with_r7.worst_r7_hazard_per_h = Some(1.5);
    with_r7.first_passed_cycle_h = Some(3.25);
    let records = vec![record("default", 1), with_r7];
    write_records(&path, &records).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(text.ends_with('\n'));
    assert_eq!(read_records(&path, "records").unwrap(), records);
}

#[test]
fn an_empty_file_and_blank_lines_hold_no_records() {
    assert!(read_text("").unwrap().is_empty());
    assert!(read_text("\n \t\n").unwrap().is_empty());
}

#[test]
fn blank_lines_between_records_are_skipped() {
    let text = format!("{}\n\n{}\n", line(&record("a", 1)), line(&record("a", 2)));
    assert_eq!(read_text(&text).unwrap().len(), 2);
}

#[test]
fn a_malformed_line_names_its_number() {
    let text = format!("{}\n{{", line(&record("a", 1)));
    let error = read_text(&text).unwrap_err().to_string();
    assert!(
        error.contains("decode baseline") && error.contains("line 2"),
        "{error}"
    );
}

#[test]
fn a_run_listed_twice_is_refused_even_across_splits() {
    let mut holdout = record("target", 1);
    holdout.split = togi_tool_bench::Split::Holdout;
    let text = format!("{}\n{}\n", line(&record("target", 1)), line(&holdout));
    let error = read_text(&text).unwrap_err().to_string();
    assert!(error.contains("duplicate baseline run target/1"), "{error}");
}

#[test]
fn a_record_is_an_object_with_exactly_the_fields_the_format_names() {
    let mut value = serde_json::to_value(record("a", 1)).unwrap();
    value["partial_seconds"] = 150.into();
    assert!(read_text(&value.to_string()).is_err());
    assert!(read_text("[\"a\",1]").is_err());
    let mut missing = serde_json::to_value(record("a", 1)).unwrap();
    missing.as_object_mut().unwrap().remove("depth");
    assert!(read_text(&missing.to_string()).is_err());
}

#[test]
fn a_missing_file_is_a_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = read_records(&dir.path().join("missing.jsonl"), "baseline").unwrap_err();
    assert!(error.to_string().contains("baseline"), "{error}");
}

#[test]
fn absent_optional_measurements_are_omitted_or_null_as_the_format_says() {
    let value = serde_json::to_value(record("a", 1)).unwrap();
    let object = value.as_object().unwrap();
    assert!(!object.contains_key("worst_r7_hazard_per_h"));
    assert!(!object.contains_key("model_check"));
    assert!(!object.contains_key("machine"));
    assert!(object["first_passed_cycle_h"].is_null());
}

#[test]
fn a_run_status_follows_the_exit_status_and_what_the_session_left() {
    let cases = [
        (
            "concluded",
            Status::of(0, true, false, false),
            Status::Concluded,
        ),
        (
            "empty journal",
            Status::of(0, false, false, false),
            Status::Error,
        ),
        (
            "dead end in the journal",
            Status::of(1, true, true, false),
            Status::DeadEnd,
        ),
        (
            "dead end in the log",
            Status::of(1, false, false, true),
            Status::DeadEnd,
        ),
        (
            "plain failure",
            Status::of(1, false, false, false),
            Status::Error,
        ),
        (
            "boot cap",
            Status::of(3, true, false, false),
            Status::Censored,
        ),
        (
            "boot cap without a journal",
            Status::of(3, false, false, false),
            Status::Error,
        ),
        (
            "usage error",
            Status::of(2, true, false, false),
            Status::Error,
        ),
        (
            "killed by a signal",
            Status::of(-1, true, false, false),
            Status::Error,
        ),
    ];
    for (name, got, want) in cases {
        assert_eq!(got, want, "{name}");
    }
}
