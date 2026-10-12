use togi_journal::fs::MemFs;
use togi_journal::{Event, SessionId, StateDir};
use togi_tool_audit::Check::{Avoidance, Termination, WriteProtocol};
use togi_tool_audit::Completion::{Finished, InProgress};
use togi_tool_audit::{AuditError, Source, audit_recordings};

use crate::fixtures::*;

fn bytes(events: &[Event]) -> Vec<u8> {
    events
        .iter()
        .flat_map(|event| {
            let mut line = event.encode().unwrap().into_bytes();
            line.push(b'\n');
            line
        })
        .collect()
}

fn journal(body: Vec<Event>) -> Vec<u8> {
    let mut events = vec![header()];
    events.extend(body);
    bytes(&events)
}

fn open(fs: &MemFs) -> StateDir {
    StateDir::simulated(fs.clone()).unwrap()
}

fn session(text: &str) -> SessionId {
    text.parse().unwrap()
}

#[test]
fn archives_may_end_anywhere_and_only_the_live_run_must_conclude() {
    let fs = MemFs::new();
    fs.put(
        "/state/archive/20251231T000000Z.jsonl",
        journal(vec![intent(2, set(0, -5)), write(3, set(0, -5), 2)]),
    );
    fs.put("/state/events.jsonl", journal(vec![stop(2)]));
    let audits = audit_recordings(&open(&fs), Finished).unwrap();
    assert_eq!(audits.len(), 2);
    assert_eq!(
        audits[0].source,
        Source::Archived(session("20251231T000000Z"))
    );
    assert!(audits.iter().all(|audit| audit.violations.is_empty()));
}

#[test]
fn a_violation_is_attributed_to_its_own_journal() {
    let fs = MemFs::new();
    fs.put(
        "/state/archive/20251231T000000Z.jsonl",
        journal(vec![event(2, "smu.write", set(0, 0))]),
    );
    fs.put(
        "/state/events.jsonl",
        journal(vec![phase(2, 0, Some(-30)), applied(3, [-30, 0])]),
    );
    let audits = audit_recordings(&open(&fs), InProgress).unwrap();
    let seen: Vec<_> = audits
        .iter()
        .map(|audit| {
            let violations: Vec<_> = audit
                .violations
                .iter()
                .map(|violation| (violation.seq.get(), violation.check))
                .collect();
            (audit.source.clone(), violations)
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (
                Source::Archived(session("20251231T000000Z")),
                vec![(2, WriteProtocol)]
            ),
            (Source::Live, vec![(3, Avoidance)]),
        ]
    );
}

#[test]
fn a_directory_with_archives_only_audits_them_and_an_unfinished_live_journal_is_open() {
    let fs = MemFs::new();
    fs.put(
        "/state/archive/20251231T000000Z.jsonl",
        journal(vec![stop(2)]),
    );
    let audits = audit_recordings(&open(&fs), Finished).unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(
        audits[0].source,
        Source::Archived(session("20251231T000000Z"))
    );

    fs.put("/state/events.jsonl", journal(vec![phase(2, 0, None)]));
    let audits = audit_recordings(&open(&fs), Finished).unwrap();
    assert_eq!(
        audits[1]
            .violations
            .iter()
            .map(|violation| violation.check)
            .collect::<Vec<_>>(),
        vec![Termination]
    );
}

#[test]
fn an_empty_directory_has_nothing_to_audit() {
    assert!(
        audit_recordings(&open(&MemFs::new()), Finished)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_journal_that_cannot_be_read_is_an_error_not_a_clean_audit() {
    let fs = MemFs::new();
    let mut text = journal(vec![]);
    text.extend_from_slice(
        br#"{"seq":2,"time":"2026-01-01T00:00:00Z","boot":"b","kind":"profile.applied","msg":"m","data":{"offsets":{"0":-51},"condition":"alone"}}"#,
    );
    text.push(b'\n');
    fs.put("/state/events.jsonl", text);
    assert!(matches!(
        audit_recordings(&open(&fs), Finished),
        Err(AuditError::Journal(_))
    ));
}

#[test]
fn an_archive_that_cannot_be_read_is_an_error_naming_the_archive() {
    let fs = MemFs::new();
    let mut text = journal(vec![]);
    text.extend_from_slice(b"{\"seq\":2}\n");
    fs.put("/state/archive/20251231T000000Z.jsonl", text);
    fs.put("/state/events.jsonl", journal(vec![stop(2)]));
    assert!(matches!(
        audit_recordings(&open(&fs), Finished),
        Err(AuditError::Archive(_))
    ));
}

#[test]
fn archives_without_a_live_journal_yield_no_live_audit() {
    let fs = MemFs::new();
    fs.put("/state/archive/20251231T000000Z.jsonl", journal(vec![]));
    fs.put("/state/archive/20260101T000000Z.jsonl", journal(vec![]));
    let sources: Vec<Source> = audit_recordings(&open(&fs), Finished)
        .unwrap()
        .into_iter()
        .map(|audit| audit.source)
        .collect();
    assert_eq!(
        sources,
        vec![
            Source::Archived(session("20251231T000000Z")),
            Source::Archived(session("20260101T000000Z")),
        ]
    );
}
