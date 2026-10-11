use std::io::{BufReader, Cursor};
use std::path::Path;

use super::*;
use crate::fs::{Fault, MemFs, Op};
use crate::testing::{begin_default, note, stamp};
use crate::journal::tests::sample;

fn recorded(fs: &MemFs) -> Vec<u8> {
    sample(fs)
}

#[test]
fn the_reader_streams_events_and_stops_at_a_torn_tail() {
    let mut bytes = recorded(&MemFs::new());
    let complete = bytes.len() as u64;
    bytes.extend_from_slice(b"{\"seq\":5");
    let mut reader = EventReader::new(bytes.as_slice());
    let events: Vec<_> = reader.by_ref().collect::<Result<_, _>>().unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(reader.accepted(), complete);
    assert_eq!(reader.torn(), Some(&b"{\"seq\":5"[..]));
    assert!(reader.next().is_none());
}

#[test]
fn a_complete_valid_line_without_its_newline_is_torn_not_committed() {
    let bytes = recorded(&MemFs::new());
    let without_newline = &bytes[..bytes.len() - 1];
    let recording = Recording::parse(without_newline).unwrap();
    assert_eq!(recording.events().len(), 3);
    assert_eq!(recording.torn().unwrap().bytes.last(), Some(&b'}'));
}

#[test]
fn the_reader_accepts_a_small_buffer() {
    let bytes = recorded(&MemFs::new());
    let reader = EventReader::new(BufReader::with_capacity(7, Cursor::new(bytes)));
    assert_eq!(reader.count(), 4);
}

#[test]
fn the_first_error_ends_the_stream() {
    let mut bytes = recorded(&MemFs::new());
    bytes.splice(0..0, b"garbage\n".iter().copied());
    let mut reader = EventReader::new(bytes.as_slice());
    assert!(matches!(reader.next(), Some(Err(ReadError::Decode { line: 1, .. }))));
    assert!(reader.next().is_none());
    assert_eq!(reader.accepted(), 0);
}

#[test]
fn recordings_index_events_by_seq() {
    let recording = Recording::parse(&recorded(&MemFs::new())).unwrap();
    for event in recording.events() {
        assert_eq!(recording.event(event.seq), Some(event));
    }
    assert!(recording.event(Seq::new(5).unwrap()).is_none());
    assert!(recording.torn().is_none());
}

#[test]
fn reading_a_recording_never_writes() {
    let fs = MemFs::new();
    let mut bytes = recorded(&fs);
    bytes.extend_from_slice(b"torn");
    fs.put("/state/events.jsonl", bytes.clone());
    let mutations = fs.mutations();
    let recording = Recording::read_in(&fs, Path::new("/state/events.jsonl")).unwrap();
    assert_eq!(recording.torn().unwrap().bytes, b"torn");
    assert_eq!(fs.mutations(), mutations);
    assert_eq!(fs.contents("/state/events.jsonl").unwrap(), bytes);
}

#[test]
fn a_recording_reports_the_file_it_could_not_read() {
    let fs = MemFs::new();
    let error = Recording::read_in(&fs, Path::new("/nowhere.jsonl")).unwrap_err();
    assert_eq!(error.path, Path::new("/nowhere.jsonl"));
    fs.put("/bad.jsonl", "nope\n");
    let error = Recording::read_in(&fs, Path::new("/bad.jsonl")).unwrap_err();
    assert!(matches!(error.source, ReadError::Decode { line: 1, .. }));
}

#[test]
fn a_read_failure_is_an_error_not_a_short_recording() {
    let fs = MemFs::new();
    fs.put("/j.jsonl", recorded(&MemFs::new()));
    fs.inject(Fault::before(Op::Read, 1));
    assert!(Recording::read_in(&fs, Path::new("/j.jsonl")).is_err());
}

fn context_line(seq: u64) -> String {
    format!(
        "{{\"seq\":{seq},\"time\":\"2023-11-14T22:13:20Z\",\"boot\":\"b\",\"kind\":\"session.context\",\"msg\":\"m\",\
         \"data\":{{\"bios_version\":\"1.0\",\"board\":\"B\",\"cpu_model\":\"C\",\"microcode\":\"M\",\"boost_limit_mhz\":4500}}}}\n"
    )
}

#[test]
fn the_first_recorded_context_is_found_by_complete_lines_only() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    journal.append(stamp(1), note("n"), &[]).unwrap();
    let mut bytes = fs.contents("/state/events.jsonl").unwrap();
    bytes.extend_from_slice(context_line(3).as_bytes());
    let found = {
        fs.put("/j.jsonl", bytes.clone());
        recorded_context_in(&fs, Path::new("/j.jsonl")).unwrap().unwrap()
    };
    assert_eq!((found.bios_version.as_str(), found.boost_limit_mhz), ("1.0", 4500));

    fs.put("/torn.jsonl", &bytes[..bytes.len() - 1]);
    assert_eq!(recorded_context_in(&fs, Path::new("/torn.jsonl")).unwrap(), None);
}

#[test]
fn a_missing_journal_has_no_context_but_a_damaged_one_is_an_error() {
    let fs = MemFs::new();
    assert_eq!(recorded_context_in(&fs, Path::new("/none.jsonl")).unwrap(), None);
    fs.put("/bad.jsonl", "damaged\n");
    assert!(recorded_context_in(&fs, Path::new("/bad.jsonl")).is_err());
}

#[test]
fn a_context_is_not_found_in_a_journal_without_one() {
    let fs = MemFs::new();
    fs.put("/j.jsonl", recorded(&MemFs::new()));
    assert_eq!(recorded_context_in(&fs, Path::new("/j.jsonl")).unwrap(), None);
}

#[test]
fn a_large_event_spans_reader_chunks() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let big = "x".repeat(300_000);
    journal.append(stamp(1), note(&big), &[]).unwrap();
    drop(journal);
    let recording = Recording::read_in(&fs, Path::new("/state/events.jsonl")).unwrap();
    assert_eq!(recording.events().len(), 2);
    let fresh = EventReader::new(BufReader::with_capacity(16, Cursor::new(fs.contents("/state/events.jsonl").unwrap())));
    assert_eq!(fresh.count(), 2);
}
