use std::io::{BufReader, Cursor};
use std::path::Path;

use test_case::test_case;

use super::*;
use crate::fs::{Fault, MemFs, Op};
use crate::journal::tests::{non_start_first_line, sample};
use crate::testing::{begin_default, note, stamp};

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
    bytes.extend_from_slice(b"{\"seq\":9,\"kind\":\"session.warning\"}\n");
    let mut reader = EventReader::new(bytes.as_slice());
    for _ in 0..4 {
        reader.next().unwrap().unwrap();
    }
    let accepted = reader.accepted();
    assert!(matches!(
        reader.next(),
        Some(Err(ReadError::Decode { line: 5, .. }))
    ));
    assert!(reader.next().is_none());
    assert_eq!(reader.accepted(), accepted);
}

#[test]
fn an_unknown_kind_is_its_own_error_with_its_line_and_kind() {
    let mut bytes = recorded(&MemFs::new());
    bytes.extend_from_slice(
        br#"{"seq":5,"time":"2026-01-01T00:00:00Z","boot":"b","kind":"future.thing","msg":"x","data":{}}"#,
    );
    bytes.push(b'\n');
    let error = EventReader::new(bytes.as_slice())
        .find_map(Result::err)
        .unwrap();
    assert!(
        matches!(&error, ReadError::UnknownKind { line: 5, source } if source.0 == "future.thing"),
        "{error:?}"
    );
    assert_eq!(error.to_string(), "line 5");
    assert_eq!(
        togi_machine::error_chain(&error),
        r#"line 5: unknown event kind "future.thing""#
    );
}

#[test]
fn a_known_kind_with_a_bad_payload_and_a_broken_line_stay_decode_errors() {
    let mut bytes = recorded(&MemFs::new());
    bytes.extend_from_slice(
        br#"{"seq":5,"time":"2026-01-01T00:00:00Z","boot":"b","kind":"shutdown","msg":"x","data":{"reason":7}}"#,
    );
    bytes.push(b'\n');
    assert!(matches!(
        EventReader::new(bytes.as_slice()).find_map(Result::err),
        Some(ReadError::Decode { line: 5, .. })
    ));
}

#[test]
fn a_decode_failure_keeps_every_level_as_a_downcastable_source() {
    let mut bytes = recorded(&MemFs::new());
    bytes.extend_from_slice(
        br#"{"seq":5,"time":"2026-01-01T00:00:00Z","boot":"b","kind":"shutdown","msg":"x","data":{"reason":7}}"#,
    );
    bytes.push(b'\n');
    let fs = MemFs::new();
    fs.put("/j.jsonl", bytes);
    let file = Recording::read_in(&fs, Path::new("/j.jsonl")).unwrap_err();
    assert_eq!(file.to_string(), "/j.jsonl");
    let read = std::error::Error::source(&file)
        .and_then(|source| source.downcast_ref::<ReadError>())
        .expect("the file error keeps the read error as its source");
    assert_eq!(read.to_string(), "line 5");
    let decode = std::error::Error::source(read)
        .and_then(|source| source.downcast_ref::<DecodeError>())
        .expect("the read error keeps the decode error as its source");
    assert_eq!(decode.to_string(), "shutdown event");
    let serde = std::error::Error::source(decode)
        .and_then(|source| source.downcast_ref::<serde_json::Error>())
        .expect("the decode error keeps the serde error as its source");
    assert_eq!(
        togi_machine::error_chain(&file),
        format!("/j.jsonl: line 5: shutdown event: {serde}")
    );
}

#[test]
fn a_line_that_is_not_utf8_names_its_line_and_keeps_the_utf8_error_as_its_source() {
    let mut bytes = recorded(&MemFs::new());
    bytes.extend_from_slice(b"\xff\xfe\n");
    let fs = MemFs::new();
    fs.put("/j.jsonl", bytes);
    let file = Recording::read_in(&fs, Path::new("/j.jsonl")).unwrap_err();
    let read = std::error::Error::source(&file)
        .and_then(|source| source.downcast_ref::<ReadError>())
        .expect("the file error keeps the read error as its source");
    assert!(matches!(read, ReadError::Utf8 { line: 5, .. }), "{read:?}");
    assert_eq!(read.to_string(), "line 5");
    let utf8 = std::error::Error::source(read)
        .and_then(|source| source.downcast_ref::<std::str::Utf8Error>())
        .expect("the read error keeps the utf-8 error as its source");
    assert_eq!(
        utf8.to_string(),
        "invalid utf-8 sequence of 1 bytes from index 0"
    );
    assert_eq!(
        togi_machine::error_chain(&file),
        "/j.jsonl: line 5: invalid utf-8 sequence of 1 bytes from index 0"
    );
}

#[test]
fn a_file_error_renders_the_path_then_the_line_then_the_cause_and_keeps_each_source() {
    let error = FileError {
        path: "/j.jsonl".into(),
        source: ReadError::UnknownKind {
            line: 5,
            source: UnknownKind("future.thing".to_owned()),
        },
    };
    assert_eq!(error.to_string(), "/j.jsonl");
    assert_eq!(
        togi_machine::error_chain(&error),
        r#"/j.jsonl: line 5: unknown event kind "future.thing""#
    );
    let read = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<ReadError>())
        .expect("the file error keeps the read error as its source");
    let kind = std::error::Error::source(read)
        .and_then(|source| source.downcast_ref::<UnknownKind>())
        .expect("the read error keeps the unknown kind as its source");
    assert_eq!(kind.0, "future.thing");
}

#[test]
fn an_open_error_over_a_file_error_renders_the_chain_once() {
    let error = crate::OpenError::Read(FileError {
        path: "/j.jsonl".into(),
        source: ReadError::Io(io::Error::other("disk gone")),
    });
    assert_eq!(
        togi_machine::error_chain(&error),
        "/j.jsonl: read: disk gone"
    );
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
    assert!(matches!(error.source, ReadError::Foreign));
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
        recorded_context_in(&fs, Path::new("/j.jsonl"))
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        (found.bios_version.as_str(), found.boost_limit_mhz),
        ("1.0", 4500)
    );

    fs.put("/torn.jsonl", &bytes[..bytes.len() - 1]);
    assert_eq!(
        recorded_context_in(&fs, Path::new("/torn.jsonl")).unwrap(),
        None
    );
}

#[test]
fn a_missing_journal_has_no_context_but_a_damaged_one_is_an_error() {
    let fs = MemFs::new();
    assert_eq!(
        recorded_context_in(&fs, Path::new("/none.jsonl")).unwrap(),
        None
    );
    fs.put("/bad.jsonl", "damaged\n");
    assert!(recorded_context_in(&fs, Path::new("/bad.jsonl")).is_err());
}

#[test]
fn a_context_is_not_found_in_a_journal_without_one() {
    let fs = MemFs::new();
    fs.put("/j.jsonl", recorded(&MemFs::new()));
    assert_eq!(
        recorded_context_in(&fs, Path::new("/j.jsonl")).unwrap(),
        None
    );
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
    let events = EventReader::new(BufReader::with_capacity(
        16,
        Cursor::new(fs.contents("/state/events.jsonl").unwrap()),
    ))
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[1].payload,
        Payload::SessionWarning(warning) if warning.operation == big
    ));
}

#[test]
fn the_first_of_two_differing_contexts_wins() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    journal.append(stamp(1), note("n"), &[]).unwrap();
    let mut bytes = fs.contents("/state/events.jsonl").unwrap();
    bytes.extend_from_slice(context_line(3).as_bytes());
    let second = context_line(4).replace("\"bios_version\":\"1.0\"", "\"bios_version\":\"2.0\"");
    bytes.extend_from_slice(second.as_bytes());
    fs.put("/j.jsonl", bytes);
    let found = recorded_context_in(&fs, Path::new("/j.jsonl"))
        .unwrap()
        .unwrap();
    assert_eq!(found.bios_version, "1.0");
}

#[test_case(non_start_first_line(&recorded(&MemFs::new())); "not session.start")]
#[test_case(b"garbage\n".to_vec(); "not json")]
#[test_case(flat_first_line(); "flat session.start")]
fn a_first_line_that_is_not_a_journal_header_is_another_builds_journal_and_nothing_after_it_is_read(
    mut bytes: Vec<u8>,
) {
    bytes.extend_from_slice(&recorded(&MemFs::new()));
    let mut reader = EventReader::new(bytes.as_slice());
    assert!(matches!(reader.next(), Some(Err(ReadError::Foreign))));
    assert!(reader.next().is_none());
    assert_eq!(reader.accepted(), 0);
    assert!(matches!(Recording::parse(&bytes), Err(ReadError::Foreign)));
    let fs = MemFs::new();
    fs.put("/j.jsonl", bytes);
    let error = recorded_context_in(&fs, Path::new("/j.jsonl")).unwrap_err();
    assert!(matches!(error.source, ReadError::Foreign));
    let text = togi_machine::error_chain(&error);
    assert!(text.starts_with("/j.jsonl: "), "{text}");
    assert!(text.contains("docs/howto.md#go-to-rust-cutover"), "{text}");
}

/// The Go build's `session.start`: its fields sit beside `kind`, with no `data` and no `build` object.
fn flat_first_line() -> Vec<u8> {
    br#"{"seq":1,"time":"2026-09-27T22:19:54Z","boot":"b","kind":"session.start","msg":"m","ruleset":3,"schema":2,"session":"20260927T221954Z","cores":[]}
"#
    .to_vec()
}

#[test]
fn a_journal_header_this_build_cannot_decode_is_not_another_builds_journal() {
    let good = recorded(&MemFs::new());
    let first = good.split_inclusive(|byte| *byte == b'\n').next().unwrap();
    let mut line: serde_json::Value = serde_json::from_slice(first).unwrap();
    line["data"]["build"]["channel"] = "later".into();
    let mut bytes = serde_json::to_vec(&line).unwrap();
    bytes.push(b'\n');
    bytes.extend_from_slice(&good[first.len()..]);

    // The classifier ignores the additive field, so a strict reader refuses the header by name, never as Foreign.
    let error = EventReader::new(bytes.as_slice())
        .next()
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(&error, ReadError::UnknownField { line: 1, kind: EventKind::SessionStart, field } if field == "data.build.channel"),
        "{error:?}"
    );
    let error = Recording::parse(&bytes).unwrap_err();
    assert!(
        matches!(&error, ReadError::UnknownField { line: 1, kind: EventKind::SessionStart, field } if field == "data.build.channel"),
        "{error:?}"
    );

    // A lenient reader accepts the same header and reports the field it ignored.
    let mut reader = EventReader::lenient(bytes.as_slice());
    assert!(reader.by_ref().all(|event| event.is_ok()));
    assert_eq!(reader.accepted(), bytes.len() as u64);
    let skipped = reader.skipped();
    assert!(skipped.events().is_empty());
    assert_eq!(skipped.ignored().len(), 1);
    assert_eq!(
        (
            skipped.ignored()[0].line,
            skipped.ignored()[0].fields.as_slice()
        ),
        (1, &["data.build.channel".to_owned()][..])
    );
}

#[test_case(
    include_str!("../../testdata/20260927T221954Z-config.jsonl").lines().next().unwrap();
    "go"
)]
#[test_case("garbage"; "garbage")]
fn an_unterminated_first_line_that_is_not_a_journal_header_is_refused_not_torn(bytes: &str) {
    // `testdata/20260927T221954Z-config.jsonl` is the Go build's journal from
    // `internal/journal/testdata`, with its boot ids replaced by synthetic ones.
    let mut reader = EventReader::new(bytes.as_bytes());
    assert!(matches!(reader.next(), Some(Err(ReadError::Foreign))));
    assert!(reader.next().is_none());
    assert_eq!(reader.accepted(), 0);
    assert!(reader.torn().is_none());
    assert!(matches!(
        Recording::parse(bytes.as_bytes()),
        Err(ReadError::Foreign)
    ));
    let fs = MemFs::new();
    fs.put("/j.jsonl", bytes.to_owned());
    let mutations = fs.mutations();
    let error = Recording::read_in(&fs, Path::new("/j.jsonl")).unwrap_err();
    assert!(matches!(error.source, ReadError::Foreign));
    assert_eq!(fs.mutations(), mutations);
    assert_eq!(fs.contents("/j.jsonl").unwrap(), bytes.as_bytes());
}

#[test]
fn an_unterminated_journal_header_is_still_a_torn_tail() {
    let bytes = recorded(&MemFs::new());
    let first = bytes.split_inclusive(|byte| *byte == b'\n').next().unwrap();
    let torn = &first[..first.len() - 1];
    let mut reader = EventReader::new(torn);
    assert!(reader.next().is_none());
    assert_eq!(reader.torn(), Some(torn));
}

/// A reader that serves `data` and then fails instead of reporting end of input.
struct FailingAfter {
    data: Vec<u8>,
    position: usize,
}

impl io::Read for FailingAfter {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let len = available.len().min(buf.len());
        buf[..len].copy_from_slice(&available[..len]);
        self.consume(len);
        Ok(len)
    }
}

impl BufRead for FailingAfter {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.position >= self.data.len() {
            return Err(io::Error::other("device went away"));
        }
        Ok(&self.data[self.position..=self.position])
    }

    fn consume(&mut self, amount: usize) {
        self.position += amount;
    }
}

#[test]
fn a_reader_that_fails_mid_line_yields_the_error_and_ends_without_a_torn_tail() {
    let mut data = recorded(&MemFs::new());
    let complete = data.len() as u64;
    data.extend_from_slice(b"{\"seq\":5,\"tim");
    let mut reader = EventReader::new(FailingAfter { data, position: 0 });
    let mut accepted = Vec::new();
    let error = loop {
        match reader
            .next()
            .expect("the failure is yielded before the end")
        {
            Ok(event) => accepted.push(event),
            Err(error) => break error,
        }
    };
    assert!(matches!(error, ReadError::Io(_)), "{error}");
    assert_eq!(accepted.len(), 4);
    assert_eq!(reader.accepted(), complete);
    assert_eq!(reader.torn(), None);
    assert!(reader.next().is_none());
    assert_eq!(reader.accepted(), complete);
}

fn started(recording: &Recording) -> crate::SessionId {
    let payload = &recording.events()[0].payload;
    let Payload::SessionStart(start) = payload else {
        panic!("first event is {payload:?}");
    };
    start.session
}

#[test]
fn recordings_read_the_archives_oldest_first_then_the_live_journal() {
    use crate::testing::{begin, boot, session, simulated, start};

    let fs = MemFs::new();
    assert!(simulated(&fs).recordings().unwrap().is_empty());

    let newer = session("20231114T000000Z");
    let older = session("20231113T000000Z");
    let dir = begin(&fs, newer)
        .archive(stamp(5), crate::ArchiveKind::Transition, &[])
        .unwrap();
    let archived = dir.recordings().unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(started(&archived[0]), newer);

    let mut live = dir.open(boot(2)).unwrap();
    live.append(stamp(6), start(older), &[]).unwrap();
    let dir = live.close().unwrap();
    let recordings = dir.recordings().unwrap();
    assert_eq!(recordings.len(), 2);
    assert_eq!(started(&recordings[0]), newer);
    assert_eq!(started(&recordings[1]), older);

    let events = recordings.into_iter().nth(1).unwrap().into_events();
    assert_eq!(events.len(), 1);
}

#[test]
fn recordings_fail_on_an_unreadable_journal_and_ignore_a_torn_tail() {
    use crate::testing::{path, simulated};

    let fs = MemFs::new();
    let mut bytes = recorded(&fs);
    bytes.extend_from_slice(b"{\"seq\":5");
    fs.put(path("events.jsonl"), bytes);
    let recordings = simulated(&fs).recordings().unwrap();
    assert_eq!(recordings[0].events().len(), 4);
    assert!(recordings[0].torn().is_some());

    let broken = MemFs::new();
    broken.put(path("events.jsonl"), "garbage\n");
    assert!(matches!(
        simulated(&broken).recordings(),
        Err(LookupError::File(_))
    ));
}

fn journal_of(session: crate::SessionId) -> Vec<u8> {
    let fs = MemFs::new();
    drop(crate::testing::begin(&fs, session));
    crate::testing::live(&fs)
}

fn archived_pair(fs: &MemFs) -> (crate::SessionId, crate::SessionId) {
    use crate::testing::session;

    let ninth = session("20231114T000000Z-9");
    let tenth = session("20231114T000000Z-10");
    for id in [tenth, ninth] {
        fs.put(format!("/state/archive/{id}.jsonl"), journal_of(id));
    }
    (ninth, tenth)
}

#[test]
fn recordings_order_archives_by_session_not_by_name() {
    use crate::testing::{path, session, simulated};

    let fs = MemFs::new();
    let (ninth, tenth) = archived_pair(&fs);
    let live = session("20231115T000000Z");
    fs.put(path("events.jsonl"), journal_of(live));
    let started: Vec<_> = simulated(&fs)
        .recordings()
        .unwrap()
        .iter()
        .map(started)
        .collect();
    assert_eq!(started, [ninth, tenth, live]);
}

fn archive_failure(fs: &MemFs) -> FileError {
    match crate::testing::simulated(fs).recordings() {
        Err(LookupError::File(error)) => error,
        other => panic!("expected a file error, got {other:?}"),
    }
}

#[test]
fn an_undecodable_archive_after_a_readable_one_is_an_error_naming_its_session() {
    let fs = MemFs::new();
    let (_, tenth) = archived_pair(&fs);
    let mut bytes = journal_of(tenth);
    bytes.extend_from_slice(b"{not json\n");
    fs.put(format!("/state/archive/{tenth}.jsonl"), bytes);
    let error = archive_failure(&fs);
    assert!(
        matches!(error.source, ReadError::Decode { line: 2, .. }),
        "{error}"
    );
    assert_eq!(
        error.path,
        Path::new(&format!("/state/archive/{tenth}.jsonl"))
    );
    assert!(error.to_string().contains(&tenth.to_string()), "{error}");
}

#[test]
fn an_unreadable_archive_after_a_readable_one_is_an_error_naming_its_session() {
    let fs = MemFs::new();
    let (_, tenth) = archived_pair(&fs);
    fs.inject(Fault::before(Op::Read, 1).named(format!("{tenth}.jsonl")));
    let error = archive_failure(&fs);
    assert!(matches!(error.source, ReadError::Io(_)), "{error}");
    assert_eq!(
        error.path,
        Path::new(&format!("/state/archive/{tenth}.jsonl"))
    );
    assert!(error.to_string().contains(&tenth.to_string()), "{error}");
}

#[test]
fn an_unreadable_live_journal_is_an_error_not_the_archives_alone() {
    use crate::testing::{path, session};

    let fs = MemFs::new();
    archived_pair(&fs);
    fs.put(
        path("events.jsonl"),
        journal_of(session("20231115T000000Z")),
    );
    fs.inject(Fault::before(Op::Read, 1).named("events.jsonl"));
    let error = archive_failure(&fs);
    assert!(matches!(error.source, ReadError::Io(_)), "{error}");
    assert_eq!(error.path, path("events.jsonl"));
}

#[test]
fn an_archived_recording_is_read_alone_and_absent_when_there_is_no_archive() {
    use crate::testing::{begin, session, simulated};

    let fs = MemFs::new();
    let id = session("20231114T000000Z");
    let dir = begin(&fs, id)
        .archive(stamp(5), crate::ArchiveKind::Transition, &[])
        .unwrap();
    let recording = dir.archived_recording(&id).unwrap().unwrap();
    assert_eq!(started(&recording), id);
    assert_eq!(recording.torn().map(|torn| torn.bytes.len()), None);
    drop(dir);
    let other = session("20231113T000000Z");
    assert!(simulated(&fs).archived_recording(&other).unwrap().is_none());
}

#[test]
fn an_archived_recording_ignores_a_torn_tail_and_names_an_unreadable_file() {
    use crate::testing::{session, simulated};

    let fs = MemFs::new();
    let id = session("20231114T000000Z");
    let mut bytes = journal_of(id);
    let complete = Recording::parse(&bytes).unwrap().events().len();
    bytes.extend_from_slice(b"{\"seq\":9");
    fs.put(format!("/state/archive/{id}.jsonl"), bytes);
    let recording = simulated(&fs).archived_recording(&id).unwrap().unwrap();
    assert_eq!(recording.events().len(), complete);
    assert!(recording.torn().is_some());

    let broken = session("20231113T000000Z");
    fs.put(format!("/state/archive/{broken}.jsonl"), "garbage\n");
    let error = simulated(&fs).archived_recording(&broken).unwrap_err();
    assert!(error.path.ends_with(format!("{broken}.jsonl")));
}

/// One complete journal line of `kind` with `data`, after the four events of [`recorded`].
fn line(seq: u64, kind: &str, cause: &str, data: &str) -> Vec<u8> {
    format!(
        "{{\"seq\":{seq},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b\",{cause}\"kind\":\"{kind}\",\"msg\":\"x\",\"data\":{data}}}\n"
    )
    .into_bytes()
}

const ARCHIVED: &str = r#"{"session":"20231114T221320Z"}"#;

fn archived_with(extra: &str) -> String {
    format!(r#"{{"session":"20231114T221320Z",{extra}}}"#)
}

fn recorded_then(lines: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = recorded(&MemFs::new());
    for line in lines {
        bytes.extend_from_slice(line);
    }
    bytes
}

#[test]
fn a_strict_reader_refuses_an_unknown_field_naming_its_line_kind_and_path() {
    let bytes = recorded_then(&[line(
        5,
        "session.archived",
        "",
        &archived_with(r#""extra":1"#),
    )]);
    let error = EventReader::new(bytes.as_slice())
        .find_map(Result::err)
        .unwrap();
    assert!(
        matches!(&error, ReadError::UnknownField { line: 5, kind: EventKind::SessionArchived, field } if field == "data.extra"),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        r#"line 5: session.archived event: unknown field "data.extra""#
    );
    let error = Recording::parse(&bytes).unwrap_err();
    assert!(matches!(error, ReadError::UnknownField { line: 5, .. }));
}

#[test]
fn a_lenient_reader_skips_unknown_kinds_and_ignores_unknown_fields_and_says_so() {
    let bytes = recorded_then(&[
        line(5, "future.thing", "", r#"{"x":1}"#),
        line(
            6,
            "session.archived",
            r#""cause":[5],"#,
            &archived_with(r#""extra":1"#),
        ),
    ]);
    let mut reader = EventReader::lenient(bytes.as_slice());
    let seqs: Vec<u64> = reader
        .by_ref()
        .map(|event| event.unwrap().seq.get())
        .collect();
    assert_eq!(seqs, [1, 2, 3, 4, 6]);
    assert_eq!(reader.accepted(), bytes.len() as u64);
    let skipped = reader.skipped();
    assert_eq!(skipped.events().len(), 1);
    assert_eq!(
        (
            skipped.events()[0].line,
            skipped.events()[0].seq.get(),
            skipped.events()[0].kind.as_str()
        ),
        (5, 5, "future.thing")
    );
    assert_eq!(skipped.ignored().len(), 1);
    assert_eq!(
        (
            skipped.ignored()[0].line,
            skipped.ignored()[0].fields.as_slice()
        ),
        (6, &["data.extra".to_owned()][..])
    );
    assert!(EventReader::new(bytes.as_slice()).skipped().is_empty());
}

#[test]
fn a_lenient_recording_finds_events_by_seq_around_a_skipped_one() {
    let bytes = recorded_then(&[
        line(5, "future.thing", "", "{}"),
        line(6, "session.archived", r#""cause":[5],"#, ARCHIVED),
        line(7, "future.thing", "", "{}"),
        line(8, "session.archived", r#""cause":[6,7],"#, ARCHIVED),
    ]);
    assert!(Recording::parse(&bytes).is_err());
    let recording = Recording::parse_with(&bytes, crate::event::Decoding::Lenient).unwrap();
    assert_eq!(recording.events().len(), 6);
    assert_eq!(recording.skipped().events().len(), 2);
    for event in recording.events() {
        assert_eq!(recording.event(event.seq), Some(event));
    }
    for skipped in [5, 7, 9] {
        assert!(
            recording.event(Seq::new(skipped).unwrap()).is_none(),
            "seq {skipped}"
        );
    }
    let sixth = recording.event(Seq::new(6).unwrap()).unwrap();
    assert_eq!(sixth.cause, [Seq::new(5).unwrap()]);
    let eighth = recording.event(Seq::new(8).unwrap()).unwrap();
    assert_eq!(eighth.cause, [Seq::new(6).unwrap(), Seq::new(7).unwrap()]);
}

#[test]
fn a_lenient_reader_still_checks_seq_causes_and_the_first_event_of_what_it_skips() {
    let gap = recorded_then(&[line(6, "future.thing", "", "{}")]);
    assert!(matches!(
        EventReader::lenient(gap.as_slice()).find_map(Result::err),
        Some(ReadError::Gap {
            line: 5,
            expected: 5,
            ..
        })
    ));

    let late = recorded_then(&[line(5, "future.thing", r#""cause":[5],"#, "{}")]);
    assert!(matches!(
        EventReader::lenient(late.as_slice()).find_map(Result::err),
        Some(ReadError::Cause { line: 5, .. })
    ));

    // A first line that is not a complete journal header is refused before it is decoded, so a lenient reader never
    // skips an unknown first kind and never reports a known one that is not session.start.
    let first = line(1, "future.thing", "", "{}");
    let error = EventReader::lenient(first.as_slice())
        .find_map(Result::err)
        .unwrap();
    assert!(matches!(&error, ReadError::Foreign), "{error:?}");

    let mut first_known = non_start_first_line(&recorded(&MemFs::new()));
    first_known.extend(line(2, "future.thing", "", "{}"));
    assert!(matches!(
        EventReader::lenient(first_known.as_slice()).next(),
        Some(Err(ReadError::Foreign))
    ));

    let bad = recorded_then(&[line(5, "shutdown", "", r#"{"reason":7}"#)]);
    assert!(matches!(
        EventReader::lenient(bad.as_slice()).find_map(Result::err),
        Some(ReadError::Decode { line: 5, .. })
    ));

    let broken = recorded_then(&[b"garbage\n".to_vec(), line(6, "future.thing", "", "{}")]);
    let mut reader = EventReader::lenient(broken.as_slice());
    assert!(matches!(
        reader.by_ref().find_map(Result::err),
        Some(ReadError::Decode { line: 5, .. })
    ));
    assert!(reader.next().is_none());
}

fn without_newline(mut line: Vec<u8>) -> Vec<u8> {
    assert_eq!(line.pop(), Some(b'\n'));
    line
}

#[test_case(include_str!("../../testdata/20260927T221954Z-config.jsonl").as_bytes(), crate::event::Decoding::Strict; "Go strict")]
#[test_case(include_str!("../../testdata/20260927T221954Z-config.jsonl").as_bytes(), crate::event::Decoding::Lenient; "Go lenient")]
#[test_case(include_str!("../../testdata/20260927T221954Z-config.jsonl").lines().next().unwrap().as_bytes(), crate::event::Decoding::Strict; "Go no newline strict")]
#[test_case(include_str!("../../testdata/20260927T221954Z-config.jsonl").lines().next().unwrap().as_bytes(), crate::event::Decoding::Lenient; "Go no newline lenient")]
#[test_case(br#"{"seq":1,"kind":"sess"#, crate::event::Decoding::Strict; "truncated strict")]
#[test_case(br#"{"seq":1,"kind":"sess"#, crate::event::Decoding::Lenient; "truncated lenient")]
#[test_case(b"garbage", crate::event::Decoding::Strict; "garbage strict")]
#[test_case(b"garbage", crate::event::Decoding::Lenient; "garbage lenient")]
#[test_case(&line(1, "future.thing", "", "{}"), crate::event::Decoding::Strict; "unknown strict")]
#[test_case(&line(1, "future.thing", "", "{}"), crate::event::Decoding::Lenient; "unknown lenient")]
#[test_case(&without_newline(line(1, "future.thing", "", "{}")), crate::event::Decoding::Strict; "unknown no newline strict")]
#[test_case(&without_newline(line(1, "future.thing", "", "{}")), crate::event::Decoding::Lenient; "unknown no newline lenient")]
fn strict_and_lenient_alike_refuse_a_first_line_that_is_not_a_complete_journal_header_and_change_nothing(
    bytes: &[u8],
    decoding: crate::event::Decoding,
) {
    let mut reader = match decoding {
        crate::event::Decoding::Strict => EventReader::new(bytes),
        crate::event::Decoding::Lenient => EventReader::lenient(bytes),
    };
    assert!(
        matches!(reader.next(), Some(Err(ReadError::Foreign))),
        "{decoding:?}"
    );
    assert!(reader.next().is_none(), "{decoding:?}");
    assert_eq!(reader.accepted(), 0, "{decoding:?}");
    assert!(reader.torn().is_none(), "{decoding:?}");
    assert!(reader.skipped().is_empty(), "{decoding:?}");
    assert!(
        matches!(
            Recording::parse_with(bytes, decoding),
            Err(ReadError::Foreign)
        ),
        "{decoding:?}"
    );
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", bytes.to_vec());
    let mutations = fs.mutations();
    let path = Path::new("/state/events.jsonl");
    for (decoding, error) in [
        (
            crate::event::Decoding::Strict,
            Recording::read_in(&fs, path).unwrap_err(),
        ),
        (
            crate::event::Decoding::Lenient,
            Recording::read_in_lenient(&fs, path).unwrap_err(),
        ),
    ] {
        assert!(matches!(error.source, ReadError::Foreign), "{decoding:?}");
    }
    assert_eq!(fs.mutations(), mutations);
    assert_eq!(fs.contents("/state/events.jsonl").unwrap(), bytes);
}

#[test]
fn the_skipped_report_is_one_line_naming_kinds_lines_and_fields() {
    let bytes = recorded_then(&[
        line(5, "future.thing", "", "{}"),
        line(6, "session.archived", "", &archived_with(r#""extra":1"#)),
        line(7, "other.kind", "", "{}"),
        line(
            8,
            "session.archived",
            "",
            &archived_with(r#""extra":1,"more":2"#),
        ),
        line(9, "future.thing", "", "{}"),
    ]);
    let recording = Recording::parse_with(&bytes, crate::event::Decoding::Lenient).unwrap();
    assert_eq!(
        recording.skipped().to_string(),
        "skipped 3 events of kinds this build does not know (future.thing at lines 5, 9; other.kind at line 7); \
         ignored unknown fields: session.archived data.extra (line 6); session.archived data.extra, data.more (line 8)"
    );
}

#[test]
fn the_skipped_report_is_empty_singular_and_escaped_as_the_case_may_be() {
    let mut report = Skipped::new();
    assert!(report.is_empty());
    assert_eq!(report.to_string(), "");
    report.skip(5, Seq::new(5).unwrap(), "future.thing".into());
    assert!(!report.is_empty());
    assert_eq!(
        report.to_string(),
        "skipped 1 event of a kind this build does not know (future.thing at line 5)"
    );
    let mut hostile = Skipped::new();
    hostile.skip(2, Seq::new(2).unwrap(), "bad\u{1b}[31mkind".into());
    hostile.ignore(
        3,
        Seq::new(3).unwrap(),
        "trial.end",
        vec!["data.a\nb".into()],
    );
    let text = hostile.to_string();
    assert!(!text.contains('\u{1b}') && !text.contains('\n'), "{text:?}");
    assert_eq!(
        text,
        r#"skipped 1 event of a kind this build does not know ("bad\u{1b}[31mkind" at line 2); ignored unknown fields: trial.end "data.a\nb" (line 3)"#
    );
}

#[test]
fn the_lenient_entry_points_read_files_and_directories() {
    use crate::testing::{session, simulated};

    let fs = MemFs::new();
    let id = session("20231113T000000Z");
    fs.put(format!("/state/archive/{id}.jsonl"), journal_of(id));
    let live = recorded_then(&[line(5, "future.thing", "", "{}")]);
    fs.put("/state/events.jsonl", live);
    let dir = simulated(&fs);

    let error = dir.recordings().unwrap_err();
    assert!(
        matches!(&error, LookupError::File(file) if matches!(file.source, ReadError::UnknownKind { line: 5, .. })),
        "{error}"
    );
    let recordings = dir.recordings_lenient().unwrap();
    assert_eq!(recordings.len(), 2);
    assert!(recordings[0].skipped().is_empty());
    assert_eq!(recordings[1].skipped().events().len(), 1);

    let path = Path::new("/state/events.jsonl");
    assert!(Recording::read_in(&fs, path).is_err());
    assert_eq!(
        Recording::read_in_lenient(&fs, path)
            .unwrap()
            .skipped()
            .events()
            .len(),
        1
    );
}
