use std::fs;
use std::path::Path;

use tempfile::tempdir;
use togi_watchtest::{Scenario, install};

use super::plant::{UNKNOWN_KIND, plant};
use crate::{Error, load};

fn installed(scenario: Scenario) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    install(dir.path(), scenario).unwrap();
    dir
}

fn rewrite_first_line(dir: &Path, from: &str, to: &str) {
    let path = dir.join("events.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let (first, rest) = text.split_once('\n').unwrap();
    assert!(first.contains(from), "{first}");
    fs::write(&path, format!("{}\n{rest}", first.replacen(from, to, 1))).unwrap();
}

#[test]
fn the_events_of_a_recording_are_read_whole() {
    let dir = installed(Scenario::Checking);
    let loaded = load::events(dir.path()).unwrap();
    assert_eq!(loaded.events, Scenario::Checking.events().unwrap());
    assert_eq!(loaded.skipped, None);
}

#[test]
fn an_unknown_kind_in_the_middle_is_skipped_unknown_fields_are_ignored_and_the_report_says_which() {
    let dir = installed(Scenario::Checking);
    let planted = plant(dir.path());
    let path = dir.path().join("events.jsonl");
    let bytes = fs::read(&path).unwrap();
    let before: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();

    let loaded = load::events(dir.path()).unwrap();
    let known: Vec<_> = Scenario::Checking
        .events()
        .unwrap()
        .into_iter()
        .filter(|event| event.seq.get() != planted.kind_line as u64)
        .collect();
    assert_eq!(loaded.events, known);
    assert!(
        loaded
            .events
            .iter()
            .any(|event| event.seq.get() > planted.kind_line as u64)
    );
    let report = loaded.skipped.unwrap();
    assert!(report.starts_with(&planted.skipped_kind()), "{report}");
    for ignored in planted.ignored_fields() {
        assert!(report.contains(&ignored), "{report}");
    }
    assert!(!report.contains('\n'), "{report}");

    assert_eq!(fs::read(&path).unwrap(), bytes);
    let after: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(after, before);
}

#[test]
fn an_unknown_kind_on_the_first_line_is_still_an_error() {
    let dir = installed(Scenario::Checking);
    rewrite_first_line(
        dir.path(),
        "\"kind\":\"session.start\"",
        &format!("\"kind\":\"{UNKNOWN_KIND}\""),
    );
    assert!(matches!(
        load::events(dir.path()).unwrap_err(),
        Error::Read(_)
    ));
}

#[test]
fn reading_leaves_the_journal_and_its_unfinished_last_line_as_they_are() {
    let dir = installed(Scenario::Checking);
    let path = dir.path().join("events.jsonl");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"{\"unfinished\":");
    fs::write(&path, &bytes).unwrap();
    let listing = || {
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = listing();
    let loaded = load::events(dir.path()).unwrap();
    assert_eq!(
        loaded.events.len(),
        Scenario::Checking.events().unwrap().len()
    );
    assert_eq!(loaded.skipped, None);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(listing(), before);
}

#[test]
fn a_recording_of_another_ruleset_or_a_newer_schema_is_refused() {
    let ruleset = installed(Scenario::Checking);
    let current = togi_tuner::RULESET;
    rewrite_first_line(
        ruleset.path(),
        &format!("\"ruleset\":{current}"),
        &format!("\"ruleset\":{}", current + 1),
    );
    let error = load::events(ruleset.path()).unwrap_err();
    assert!(matches!(error, Error::Refused(_)), "{error}");
    assert!(error.to_string().contains("ruleset"), "{error}");

    let schema = installed(Scenario::Checking);
    let current = togi_journal::SCHEMA;
    rewrite_first_line(
        schema.path(),
        &format!("\"schema\":{current}"),
        &format!("\"schema\":{}", current + 1),
    );
    let error = load::events(schema.path()).unwrap_err();
    assert!(matches!(error, Error::Refused(_)), "{error}");
    assert!(error.to_string().contains("schema"), "{error}");
}

#[test]
fn a_missing_journal_and_an_empty_one_are_errors_naming_what_is_wrong() {
    let dir = tempdir().unwrap();
    let error = load::events(dir.path()).unwrap_err();
    assert!(matches!(error, Error::Read(_)), "{error}");
    assert!(error.to_string().starts_with("read journal: "), "{error}");

    fs::write(dir.path().join("events.jsonl"), "").unwrap();
    let error = load::events(dir.path()).unwrap_err();
    assert!(matches!(error, Error::Empty(_)), "{error}");
    assert!(error.to_string().ends_with(" holds no events"), "{error}");
}

#[test]
fn a_journal_that_does_not_decode_is_an_error() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("events.jsonl"), "not an event\n").unwrap();
    assert!(matches!(
        load::events(dir.path()).unwrap_err(),
        Error::Read(_)
    ));
}

#[test]
fn a_refusal_comes_before_any_later_event_is_decoded() {
    let dir = installed(Scenario::Checking);
    let current = togi_tuner::RULESET;
    rewrite_first_line(
        dir.path(),
        &format!("\"ruleset\":{current}"),
        &format!("\"ruleset\":{}", current + 1),
    );
    let path = dir.path().join("events.jsonl");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("not an event\n");
    fs::write(&path, text).unwrap();
    assert!(matches!(
        load::events(dir.path()).unwrap_err(),
        Error::Refused(_)
    ));
}
