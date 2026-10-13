use std::fmt::Write;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};
use togi_journal::{Event, Seq};
use togi_tuner::RULESET;

use super::support::{A, B, baseline, build, crash, events, id, start, start_with};
use crate::{Anchor, Error, Session, read_dir, read_dir_lenient};

const C: &str = "20261007T020000Z";
const D: &str = "20261007T020000Z-2";

fn lines(events: &[Event]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, event).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn journal(session: &str, extra: usize) -> Vec<u8> {
    let mut payloads = start(session, "abc1234");
    payloads.extend((0..extra).map(|_| crash()));
    lines(&events(1, payloads))
}

fn archive(dir: &Path, name: &str, bytes: &[u8]) {
    fs::create_dir_all(dir.join("archive")).unwrap();
    fs::write(dir.join("archive").join(name), bytes).unwrap();
}

fn ids(sessions: &[Session]) -> Vec<String> {
    sessions
        .iter()
        .map(|session| session.id().to_string())
        .collect()
}

#[test]
fn a_directory_reads_archives_oldest_first_then_the_live_journal() {
    let dir = tempfile::tempdir().unwrap();
    archive(dir.path(), &format!("{B}.jsonl"), &journal(B, 1));
    archive(dir.path(), &format!("{A}.jsonl"), &journal(A, 0));
    archive(dir.path(), &format!("{D}.jsonl"), &journal(D, 0));
    fs::write(dir.path().join("events.jsonl"), journal(C, 2)).unwrap();
    let sessions = read_dir(dir.path(), None).unwrap();
    assert_eq!(ids(&sessions), [A, B, D, C]);
}

#[test]
fn archives_older_than_the_anchor_session_are_skipped_unread() {
    let dir = tempfile::tempdir().unwrap();
    archive(
        dir.path(),
        &format!("{A}.jsonl"),
        b"older than the anchor and unreadable\n",
    );
    archive(dir.path(), &format!("{B}.jsonl"), &journal(B, 0));
    fs::write(dir.path().join("events.jsonl"), journal(C, 0)).unwrap();
    let sessions = read_dir(dir.path(), Some(id(B))).unwrap();
    assert_eq!(ids(&sessions), [B, C]);
    assert!(matches!(read_dir(dir.path(), None), Err(Error::Journal(_))));
}

#[test]
fn entries_that_are_not_journals_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    archive(dir.path(), "notes.jsonl", b"not a session\n");
    archive(dir.path(), &format!("{A}.carry-pending"), b"");
    fs::create_dir_all(dir.path().join("archive").join(format!("{B}.jsonl"))).unwrap();
    archive(dir.path(), &format!("{C}.jsonl"), b"");
    assert!(read_dir(dir.path(), None).unwrap().is_empty());
}

#[test]
fn a_directory_without_journals_holds_no_session() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_dir(dir.path(), None).unwrap().is_empty());
    assert!(
        read_dir(dir.path().join("missing"), None)
            .unwrap()
            .is_empty()
    );
    fs::write(dir.path().join("events.jsonl"), b"").unwrap();
    assert!(read_dir(dir.path(), None).unwrap().is_empty());
}

#[test]
fn an_archive_that_cannot_be_listed_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("archive"), b"a file, not a directory").unwrap();
    let error = read_dir(dir.path(), None).unwrap_err();
    assert!(matches!(error, Error::ListArchive { .. }), "{error}");
    assert!(error.to_string().starts_with("list archive "), "{error}");
}

#[test]
fn a_journal_that_fails_the_readers_checks_is_an_error_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("events.jsonl"), b"{\"seq\": 1}\n").unwrap();
    let error = read_dir(dir.path(), None).unwrap_err();
    assert!(matches!(error, Error::Journal(_)));
    assert!(error.to_string().contains("events.jsonl"), "{error}");
    let mut gap = events(1, start(A, "abc1234"));
    gap.extend(events(5, [crash()]));
    fs::write(dir.path().join("events.jsonl"), lines(&gap)).unwrap();
    assert!(matches!(read_dir(dir.path(), None), Err(Error::Journal(_))));
}

#[test]
fn a_lenient_read_skips_an_event_of_an_unknown_kind_and_hashes_the_file_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let before = events(1, start(A, "abc1234"));
    let skipped_seq = before.len() as u64 + 1;
    let mut bytes = lines(&before);
    let unknown = format!(
        "{{\"seq\":{skipped_seq},\"time\":\"{}\",\"boot\":\"boot\",\"kind\":\"future.thing\",\"msg\":\"x\",\"data\":{{}}}}\n",
        before[0].time
    );
    bytes.extend_from_slice(unknown.as_bytes());
    let after = events(skipped_seq + 1, [crash()]);
    bytes.extend(lines(&after));
    fs::write(dir.path().join("events.jsonl"), &bytes).unwrap();

    assert!(matches!(read_dir(dir.path(), None), Err(Error::Journal(_))));
    let sessions = read_dir_lenient(dir.path(), None).unwrap();
    let session = &sessions[0];
    assert_eq!(session.events().len(), before.len() + 1);
    let skipped = session.skipped();
    assert_eq!(skipped.events().len(), 1);
    assert_eq!(skipped.events()[0].seq.get(), skipped_seq);
    let last = after[0].seq;
    let anchor = session.anchor_at(last).unwrap();
    let digest = Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
    assert_eq!(anchor.sha256, digest);
}

#[test]
fn a_journal_that_does_not_open_with_session_start_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("events.jsonl"),
        lines(&events(1, [crash()])),
    )
    .unwrap();
    assert!(matches!(read_dir(dir.path(), None), Err(Error::Journal(_))));
    assert!(matches!(
        Session::from_events(events(1, [crash()])),
        Err(Error::NotStarted)
    ));
    assert!(matches!(
        Session::from_events(Vec::new()),
        Err(Error::NotStarted)
    ));
}

#[test]
fn the_anchor_is_the_last_complete_event_of_the_live_journal() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = journal(A, 2);
    bytes.extend_from_slice(b"{\"seq\":5,\"time\":\"torn");
    fs::write(dir.path().join("events.jsonl"), &bytes).unwrap();
    let anchor = Anchor::of(dir.path()).unwrap();
    assert_eq!((anchor.session, anchor.seq), (id(A), Seq::new(4).unwrap()));
    let whole = Session::from_events(events(1, {
        let mut payloads = start(A, "abc1234");
        payloads.extend([crash(), crash()]);
        payloads
    }))
    .unwrap();
    assert_eq!(whole.anchor_at(Seq::new(4).unwrap()), Some(anchor));
}

#[test]
fn the_anchor_of_an_empty_or_missing_live_journal_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = Anchor::of(dir.path()).unwrap_err();
    assert!(matches!(error, Error::ReadJournal { .. }), "{error}");
    assert!(error.to_string().starts_with("read journal "), "{error}");
    fs::write(dir.path().join("events.jsonl"), b"").unwrap();
    let error = Anchor::of(dir.path()).unwrap_err();
    assert!(matches!(error, Error::NoEvents { .. }), "{error}");
    // A first line that is not a complete journal header is refused, even when a crash may have torn it.
    fs::write(dir.path().join("events.jsonl"), b"{\"seq\":1,\"torn").unwrap();
    let error = Anchor::of(dir.path()).unwrap_err();
    assert!(
        matches!(&error, Error::Journal(file) if matches!(file.source, togi_journal::ReadError::Foreign)),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("first line is not a complete header of this build"),
        "{error}"
    );
}

#[test]
fn the_hash_covers_the_lines_as_the_file_holds_them() {
    let dir = tempfile::tempdir().unwrap();
    let all = events(1, {
        let mut payloads = start(A, "abc1234");
        payloads.push(crash());
        payloads
    });
    let reordered: Vec<u8> = all
        .iter()
        .flat_map(|event| {
            let value: serde_json::Value = serde_json::to_value(event).unwrap();
            let mut line = serde_json::to_vec(&value).unwrap();
            line.push(b'\n');
            line
        })
        .collect();
    assert_ne!(
        reordered,
        lines(&all),
        "keys sort differently from the struct's"
    );
    fs::write(dir.path().join("events.jsonl"), &reordered).unwrap();
    let anchor = Anchor::of(dir.path()).unwrap();
    let digest = Sha256::digest(&reordered)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        });
    assert_eq!(anchor.sha256, digest);
    let canonical = Session::from_events(all)
        .unwrap()
        .anchor_at(Seq::new(3).unwrap())
        .unwrap();
    assert_ne!(anchor.sha256, canonical.sha256);
    let sessions = read_dir(dir.path(), None).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(crate::after(&sessions, &anchor).unwrap().crashes, 0);
}

#[test]
fn an_appended_event_changes_no_prefix_hash_but_an_edited_one_changes_every_later_hash() {
    let short = Session::from_events(events(1, start(A, "abc1234"))).unwrap();
    let mut longer = start(A, "abc1234");
    longer.push(crash());
    let longer = Session::from_events(events(1, longer)).unwrap();
    let two = Seq::new(2).unwrap();
    assert_eq!(short.anchor_at(two), longer.anchor_at(two));
    assert_ne!(short.anchor_at(two), longer.anchor_at(Seq::new(3).unwrap()));
    assert_eq!(short.anchor_at(Seq::new(9).unwrap()), None);
    let other = Session::from_events(events(
        1,
        [start_with(A, build("fedcba9")), start(A, "x").remove(1)],
    ))
    .unwrap();
    assert_ne!(other.anchor_at(two), short.anchor_at(two));
}

#[test]
fn an_anchor_serializes_as_plain_json() {
    let session = Session::from_events(events(1, start(A, "abc1234"))).unwrap();
    let anchor = session.anchor_at(Seq::new(2).unwrap()).unwrap();
    let json = serde_json::to_value(&anchor).unwrap();
    assert_eq!(json["session"], A);
    assert_eq!(json["seq"], 2);
    assert_eq!(json["time"], "2026-10-07T02:00:00Z");
    assert_eq!(json["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(serde_json::from_value::<Anchor>(json).unwrap(), anchor);
}

#[test]
fn an_event_that_cannot_be_encoded_is_an_error() {
    let mut payloads = events(1, start(A, "abc1234"));
    payloads[1].mono = Some(std::time::Duration::MAX);
    let error = Session::from_events(payloads).unwrap_err();
    assert!(
        matches!(error, Error::Encode { seq, .. } if seq == Seq::new(2).unwrap()),
        "{error}"
    );
    assert!(error.to_string().starts_with("encode event 2: "), "{error}");
}

#[test]
fn a_journal_of_another_ruleset_reads_as_one_session_that_cannot_be_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let mut stale = build("abc1234");
    stale.ruleset = RULESET - 1;
    let payloads = [start_with(A, stale), baseline([-10, -20])];
    fs::write(dir.path().join("events.jsonl"), lines(&events(1, payloads))).unwrap();
    let sessions = read_dir(dir.path(), None).unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(!sessions[0].replayable());
    let anchor = Anchor::of(dir.path()).unwrap();
    let error = crate::after(&sessions, &anchor).unwrap_err();
    assert!(matches!(error, Error::NotReplayable { .. }), "{error}");
    assert_eq!(
        error.to_string(),
        format!(
            "session {A} cannot be replayed: the journal's ruleset is {}; this build's is {RULESET}",
            RULESET - 1
        )
    );
}
