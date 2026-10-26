use std::io::{self, BufReader, Read};
use std::path::Path;

use togi_watchtest::Scenario;

use super::plant::{UNKNOWN_KIND, plant, recording};
use crate::{Error, load};

fn loaded(text: &str) -> Result<load::Loaded, Error> {
    load::events_from(Path::new("state"), text.as_bytes())
}

fn rewrite_first_line(text: &mut String, from: &str, to: &str) {
    let (first, rest) = text.split_once('\n').unwrap();
    assert!(first.contains(from), "{first}");
    *text = format!("{}\n{rest}", first.replacen(from, to, 1));
}

#[test]
fn the_events_of_a_recording_are_read_whole() {
    let loaded = loaded(&recording(Scenario::Controls)).unwrap();
    assert_eq!(loaded.events, Scenario::Controls.events().unwrap());
    assert_eq!(loaded.skipped, None);
}

#[test]
fn an_unknown_kind_in_the_middle_is_skipped_unknown_fields_are_ignored_and_the_report_says_which() {
    let mut text = recording(Scenario::Between);
    let planted = plant(&mut text);
    let before = text.clone();
    let loaded = loaded(&text).unwrap();
    let known: Vec<_> = Scenario::Between
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
    assert_eq!(text, before);
}

#[test]
fn an_unknown_kind_on_the_first_line_is_still_an_error() {
    let mut text = recording(Scenario::Controls);
    rewrite_first_line(
        &mut text,
        "\"kind\":\"session.start\"",
        &format!("\"kind\":\"{UNKNOWN_KIND}\""),
    );
    assert!(matches!(loaded(&text).unwrap_err(), Error::Read(_)));
}

#[test]
fn reading_leaves_the_journal_and_its_unfinished_last_line_as_they_are() {
    let mut text = recording(Scenario::Controls);
    text.push_str("{\"unfinished\":");
    let before = text.clone();
    let loaded = loaded(&text).unwrap();
    assert_eq!(
        loaded.events.len(),
        Scenario::Controls.events().unwrap().len()
    );
    assert_eq!(loaded.skipped, None);
    assert_eq!(text, before);
}

#[test]
fn a_recording_of_another_ruleset_or_a_newer_schema_is_refused() {
    let mut ruleset = recording(Scenario::Controls);
    let current = togi_tuner::RULESET;
    rewrite_first_line(
        &mut ruleset,
        &format!("\"ruleset\":{current}"),
        &format!("\"ruleset\":{}", current + 1),
    );
    let error = loaded(&ruleset).unwrap_err();
    assert!(matches!(error, Error::Refused(_)), "{error}");
    assert!(
        togi_machine::error_chain(&error).contains("ruleset is"),
        "{error}"
    );

    let mut schema = recording(Scenario::Controls);
    let current = togi_journal::SCHEMA;
    rewrite_first_line(
        &mut schema,
        &format!("\"schema\":{current}"),
        &format!("\"schema\":{}", current + 1),
    );
    let error = loaded(&schema).unwrap_err();
    assert!(matches!(error, Error::Refused(_)), "{error}");
    assert!(
        togi_machine::error_chain(&error).contains("schema"),
        "{error}"
    );
}

pub(super) struct Missing;

impl Read for Missing {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::NotFound, "missing recording"))
    }
}

#[test]
fn a_missing_journal_and_an_empty_one_are_errors_naming_what_is_wrong() {
    let error = load::events_from(Path::new("state"), BufReader::new(Missing)).unwrap_err();
    assert!(matches!(error, Error::Read(_)), "{error}");
    assert_eq!(
        togi_machine::error_chain(&error),
        "read journal: state/events.jsonl: read: missing recording"
    );

    let error = loaded("").unwrap_err();
    assert!(matches!(error, Error::Empty(_)), "{error}");
    assert_eq!(error.to_string(), "state holds no events");
}

#[test]
fn a_journal_that_does_not_decode_is_an_error() {
    let text = recording(Scenario::Controls);
    let first = text.lines().next().unwrap();
    let error = loaded(&format!("{first}\nnot an event\n")).unwrap_err();
    assert!(matches!(error, Error::Read(_)));
    assert_eq!(
        togi_machine::error_chain(&error),
        "read journal: state/events.jsonl: line 2: malformed event: expected ident at line 1 column 2"
    );
}

#[test]
fn a_refusal_comes_before_any_later_event_is_decoded() {
    let mut text = recording(Scenario::Controls);
    let current = togi_tuner::RULESET;
    rewrite_first_line(
        &mut text,
        &format!("\"ruleset\":{current}"),
        &format!("\"ruleset\":{}", current + 1),
    );
    text.push_str("not an event\n");
    assert!(matches!(loaded(&text).unwrap_err(), Error::Refused(_)));
}
