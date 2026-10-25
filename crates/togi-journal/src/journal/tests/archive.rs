use test_case::test_case;

use super::sample;
use crate::fs::{Fault, MemFs, Op};
use crate::testing::{
    begin, begin_default, boot, default_session, kinds, live, note, reopen, session, simulated,
    stamp, start,
};
use crate::{
    AppendError, ArchiveError, ArchiveKind, DirError, EventKind, ImportMarker, Marker, Recording,
    SessionId,
};

const ARCHIVED: &str = "/state/archive/20231114T221320Z.jsonl";
const TRIALS: &str = "/state/archive/20231114T221320Z-trials";
const IMPORT: &str = "/state/archive/20231114T221320Z-import-pending";
const IMPORT_TMP: &str = "/state/archive/.import-pending.tmp";
const RESET: &str = "/state/archive/20231114T221320Z-reset-all";
const COMPAT: &str = "/state/archive/20231114T221320Z-compat-pending";

fn seeded(fs: &MemFs) -> crate::Journal {
    let mut journal = begin_default(fs);
    journal.append(stamp(1), note("work"), &[]).unwrap();
    fs.put("/state/state.json", "{}\n");
    fs.put("/state/trials/0001/sample.csv", "t,v\n");
    journal
}

fn reference(kind: ArchiveKind) -> Vec<u8> {
    let fs = MemFs::new();
    seeded(&fs).archive(stamp(5), kind, &[]).unwrap();
    fs.contents(ARCHIVED).unwrap()
}

fn assert_archived_intact(fs: &MemFs, expected: &[u8], context: &str) {
    assert_eq!(fs.contents(ARCHIVED).unwrap(), expected, "{context}");
    assert_eq!(
        fs.contents(format!("{TRIALS}/0001/sample.csv")).unwrap(),
        b"t,v\n",
        "{context}"
    );
    assert!(live(fs).is_empty(), "{context}");
    assert!(fs.contents("/state/state.json").is_none(), "{context}");
    assert!(fs.kind_is_none("/state/trials"), "{context}");
    assert_eq!(fs.contents(COMPAT), None, "{context}");
}

trait Probe {
    fn kind_is_none(&self, path: &str) -> bool;
}

impl Probe for MemFs {
    fn kind_is_none(&self, path: &str) -> bool {
        use crate::fs::Fs as _;
        self.kind(std::path::Path::new(path)).unwrap().is_none()
    }
}

#[test]
fn reset_all_archives_the_session_behind_a_durable_boundary() {
    let fs = MemFs::new();
    let dir = seeded(&fs)
        .archive(stamp(5), ArchiveKind::ResetAll, &[])
        .unwrap();
    let archived = fs.contents(ARCHIVED).unwrap();
    assert_archived_intact(&fs, &archived, "reset");
    let recording = Recording::parse(&archived).unwrap();
    assert_eq!(
        kinds(recording.events()),
        ["session.start", "session.warning", "session.archived"]
    );
    assert_eq!(dir.reset_boundary().unwrap(), Some(default_session()));
    assert_eq!(dir.pending_import().unwrap(), None);
    assert_eq!(dir.archives().unwrap(), [default_session()]);
    assert!(fs.contents(RESET).is_some());
}

fn transition(destination: &str) -> ArchiveKind {
    ArchiveKind::Transition {
        destination: session(destination),
    }
}

#[test]
fn a_transition_leaves_a_pending_import_naming_its_destination_and_no_boundary() {
    let fs = MemFs::new();
    let dir = seeded(&fs)
        .archive(stamp(5), transition("20231115T000000Z"), &[])
        .unwrap();
    assert_eq!(
        dir.pending_import().unwrap(),
        Some(ImportMarker {
            source: default_session(),
            destination: session("20231115T000000Z"),
        })
    );
    assert_eq!(dir.reset_boundary().unwrap(), None);
    assert_eq!(fs.contents(IMPORT).unwrap(), b"20231115T000000Z\n");
    assert_eq!(fs.contents(IMPORT_TMP), None);
}

#[test]
fn a_transition_keeps_an_earlier_pending_import_as_the_source_and_names_the_new_destination() {
    let fs = MemFs::new();
    let dir = seeded(&fs)
        .archive(stamp(5), transition("20231115T000000Z"), &[])
        .unwrap();
    drop(dir);
    let mut second = begin(&fs, session("20231115T000000Z"));
    second.append(stamp(6), note("more"), &[]).unwrap();
    let dir = second
        .archive(stamp(7), transition("20231116T000000Z"), &[])
        .unwrap();
    assert_eq!(
        dir.pending_import().unwrap(),
        Some(ImportMarker {
            source: default_session(),
            destination: session("20231116T000000Z"),
        })
    );
    assert_eq!(dir.markers().unwrap().len(), 1);
    assert_eq!(dir.archives().unwrap().len(), 2);
}

#[test]
fn the_next_session_starts_empty_and_the_archive_stays_immutable() {
    let fs = MemFs::new();
    let dir = seeded(&fs)
        .archive(stamp(5), ArchiveKind::ResetAll, &[])
        .unwrap();
    let archived = fs.contents(ARCHIVED).unwrap();
    let id = dir.session_id(stamp(5).time).unwrap();
    assert!(id > default_session());
    let mut journal = dir.open(boot(2)).unwrap();
    assert!(journal.events().is_empty());
    assert!(matches!(
        journal.append(stamp(6), note("x"), &[]),
        Err(AppendError::FirstEvent { .. })
    ));
    journal.append(stamp(6), start(id), &[]).unwrap();
    journal.append(stamp(7), note("y"), &[]).unwrap();
    assert_eq!(fs.contents(ARCHIVED).unwrap(), archived);
}

#[test]
fn an_archive_that_already_exists_is_refused_before_anything_is_recorded() {
    let fs = MemFs::new();
    let journal = seeded(&fs);
    fs.put(ARCHIVED, "someone else's");
    let before = live(&fs);
    let mutations = fs.mutations();
    let error = journal
        .archive(stamp(5), ArchiveKind::ResetAll, &[])
        .unwrap_err();
    assert!(matches!(
        error,
        ArchiveError::Dir(DirError::ArchiveExists { .. })
    ));
    assert_eq!(fs.mutations(), mutations);
    assert_eq!(live(&fs), before);
    assert_eq!(fs.contents(ARCHIVED).unwrap(), b"someone else's");
}

#[test]
fn an_archive_target_that_is_a_directory_is_refused_before_anything_is_recorded() {
    let fs = MemFs::new();
    let journal = seeded(&fs);
    fs.put(format!("{ARCHIVED}/kept"), "x");
    let before = live(&fs);
    let mutations = fs.mutations();
    let error = journal
        .archive(stamp(5), ArchiveKind::ResetAll, &[])
        .unwrap_err();
    assert!(matches!(
        error,
        ArchiveError::Dir(DirError::ArchiveExists { .. })
    ));
    assert_eq!(fs.mutations(), mutations);
    assert_eq!(live(&fs), before);
    assert_eq!(fs.contents(format!("{ARCHIVED}/kept")).unwrap(), b"x");
}

#[test]
fn a_journal_with_no_session_has_nothing_to_archive() {
    let fs = MemFs::new();
    let journal = simulated(&fs).open(boot(1)).unwrap();
    assert!(matches!(
        journal.archive(stamp(1), ArchiveKind::ResetAll, &[]),
        Err(ArchiveError::NoSession)
    ));
}

#[test]
fn a_poisoned_journal_cannot_archive() {
    let fs = MemFs::new();
    let mut journal = seeded(&fs);
    fs.inject(Fault::before(crate::fs::Op::Write, 1));
    assert!(journal.append(stamp(2), note("x"), &[]).is_err());
    let error = journal
        .archive(stamp(5), ArchiveKind::ResetAll, &[])
        .unwrap_err();
    assert!(matches!(
        error,
        ArchiveError::Append(AppendError::Poisoned(_))
    ));
}

#[test]
fn every_archive_step_failure_resumes_once_and_leaves_the_sources_immutable() {
    for kind in [ArchiveKind::ResetAll, transition("20231115T000000Z")] {
        let expected = reference(kind);
        for nth in 1..=60 {
            for fault in [Fault::any_before(nth), Fault::any_after(nth)] {
                let fs = MemFs::new();
                let journal = seeded(&fs);
                let original = live(&fs);
                fs.inject(fault.clone());
                let result = journal.archive(stamp(5), kind, &[]).map(drop);
                fs.clear_faults();
                let context = format!("{kind:?} {fault:?}");
                let reopened = reopen(&fs, 2);
                if fs.contents(ARCHIVED).is_some() {
                    assert_archived_intact(&fs, &expected, &context);
                    assert!(reopened.events().is_empty(), "{context}");
                    match kind {
                        ArchiveKind::Transition { .. } => {
                            assert!(fs.contents(IMPORT).is_some(), "{context}");
                        }
                        ArchiveKind::ResetAll => assert!(fs.contents(RESET).is_some(), "{context}"),
                    }
                } else {
                    assert!(result.is_err(), "{context}");
                    assert_eq!(
                        live(&fs),
                        original,
                        "{context}: an archive that did not start changes nothing"
                    );
                    assert_eq!(reopened.events().len(), 2, "{context}");
                    assert!(
                        fs.contents(IMPORT).is_none(),
                        "{context}: no carry for a session that lives on"
                    );
                }
                let archived_events = fs
                    .contents(ARCHIVED)
                    .map(|bytes| Recording::parse(&bytes).unwrap())
                    .map(|recording| {
                        recording
                            .events()
                            .iter()
                            .filter(|e| e.kind() == EventKind::SessionArchived)
                            .count()
                    });
                assert!(archived_events.is_none_or(|count| count == 1), "{context}");
                if result.is_ok() {
                    assert_archived_intact(&fs, &expected, &context);
                }
            }
        }
    }
}

fn unreadable_fixture(fs: &MemFs) -> Vec<u8> {
    let mut journal = seeded(fs);
    journal.append(stamp(2), note("older build"), &[]).unwrap();
    journal.close().unwrap();
    live(fs)
}

#[test_case(ArchiveKind::ResetAll; "reset all")]
#[test_case(transition("20231115T000000Z"); "transition")]
fn an_unreadable_journal_is_archived_byte_for_byte_behind_a_compat_marker(kind: ArchiveKind) {
    let fs = MemFs::new();
    let original = unreadable_fixture(&fs);
    let mut dir = simulated(&fs);
    assert_eq!(
        dir.archive_unreadable(kind).unwrap(),
        Some(default_session())
    );
    assert_archived_intact(&fs, &original, "unreadable");
    assert!(
        dir.markers()
            .unwrap()
            .iter()
            .all(|(marker, _)| *marker != Marker::CompatPending)
    );
}

#[test]
fn archiving_nothing_archives_nothing() {
    let fs = MemFs::new();
    let mut dir = simulated(&fs);
    assert_eq!(dir.archive_unreadable(ArchiveKind::ResetAll).unwrap(), None);
    fs.put("/state/events.jsonl", "{\"seq\":1,\"ti");
    assert!(matches!(
        dir.archive_unreadable(ArchiveKind::ResetAll),
        Err(crate::DirError::ForeignJournal { .. })
    ));
    assert_eq!(
        fs.contents("/state/events.jsonl").unwrap(),
        b"{\"seq\":1,\"ti"
    );
}

#[test]
fn an_unreadable_archive_resumes_through_every_step_failure() {
    for kind in [ArchiveKind::ResetAll, transition("20231115T000000Z")] {
        for nth in 1..=50 {
            for fault in [Fault::any_before(nth), Fault::any_after(nth)] {
                let fs = MemFs::new();
                let original = unreadable_fixture(&fs);
                let mut dir = simulated(&fs);
                fs.inject(fault.clone());
                let result = dir.archive_unreadable(kind);
                fs.clear_faults();
                drop(dir);
                let context = format!("{kind:?} {fault:?}");
                let reopened = reopen(&fs, 2);
                if fs.contents(ARCHIVED).is_some() {
                    assert_archived_intact(&fs, &original, &context);
                    assert!(reopened.events().is_empty(), "{context}");
                } else {
                    assert!(result.is_err(), "{context}");
                    assert_eq!(live(&fs), original, "{context}");
                    assert!(fs.contents(IMPORT).is_none(), "{context}");
                }
                assert_eq!(fs.contents(COMPAT), None, "{context}");
            }
        }
    }
}

#[test]
fn an_archive_recorded_before_the_unreadable_path_runs_is_only_finished() {
    let fs = MemFs::new();
    let journal = seeded(&fs);
    fs.inject(Fault::before(crate::fs::Op::Rename, 1));
    assert!(
        journal
            .archive(stamp(5), ArchiveKind::ResetAll, &[])
            .is_err()
    );
    fs.clear_faults();
    let recorded = live(&fs);
    assert_eq!(Recording::parse(&recorded).unwrap().events().len(), 3);
    let mut dir = simulated(&fs);
    assert_eq!(
        dir.archive_unreadable(transition("20231115T000000Z"))
            .unwrap(),
        Some(default_session())
    );
    assert_eq!(fs.contents(ARCHIVED).unwrap(), recorded);
    assert_eq!(fs.contents(IMPORT), None);
    assert_eq!(fs.contents(COMPAT), None);
}

#[test]
fn open_finishes_a_recorded_archive_even_when_a_torn_tail_follows_it() {
    let fs = MemFs::new();
    let journal = seeded(&fs);
    fs.inject(Fault::before(crate::fs::Op::Rename, 2));
    assert!(
        journal
            .archive(stamp(5), transition("20231115T000000Z"), &[])
            .is_err()
    );
    fs.clear_faults();
    let mut bytes = live(&fs);
    bytes.extend_from_slice(b"{\"seq\":4,\"tim");
    fs.put("/state/events.jsonl", bytes.clone());
    let journal = reopen(&fs, 2);
    assert!(journal.events().is_empty());
    assert_eq!(fs.contents(ARCHIVED).unwrap(), bytes);
    assert!(fs.contents("/state/events.jsonl").is_none() || live(&fs).is_empty());
}

#[test]
fn a_recorded_archive_naming_another_session_is_refused() {
    let fs = MemFs::new();
    let journal = seeded(&fs);
    fs.inject(Fault::before(crate::fs::Op::Rename, 1));
    assert!(
        journal
            .archive(stamp(5), ArchiveKind::ResetAll, &[])
            .is_err()
    );
    fs.clear_faults();
    let text = String::from_utf8(live(&fs)).unwrap();
    let last = text.rfind("\"session\":\"20231114T221320Z\"").unwrap();
    let forged = format!(
        "{}\"session\":\"20231115T000000Z\"{}",
        &text[..last],
        &text[last + 28..]
    );
    fs.put("/state/events.jsonl", forged);
    assert!(matches!(
        simulated(&fs).open(boot(2)),
        Err(crate::OpenError::ArchiveMismatch { .. })
    ));
}

#[test]
fn a_compat_marker_without_its_archive_is_refused_and_kept() {
    let fs = MemFs::new();
    fs.put(COMPAT, "");
    let error = simulated(&fs).open(boot(1)).unwrap_err();
    assert!(matches!(
        error,
        crate::OpenError::Dir(DirError::MissingArchive { .. })
    ));
    assert!(fs.contents(COMPAT).is_some());
}

#[test]
fn a_compat_marker_after_the_journal_moved_is_removed_by_open() {
    let fs = MemFs::new();
    fs.put(COMPAT, "");
    fs.put(ARCHIVED, "moved");
    let journal = reopen(&fs, 1);
    assert!(journal.events().is_empty());
    assert_eq!(fs.contents(COMPAT), None);
    assert_eq!(fs.contents(ARCHIVED).unwrap(), b"moved");
}

#[test]
fn a_compat_marker_for_a_different_live_session_is_refused() {
    let fs = MemFs::new();
    sample(&fs);
    fs.put("/state/archive/20991231T000000Z-compat-pending", "");
    assert!(simulated(&fs).open(boot(2)).is_err());
    assert!(fs.contents("/state/events.jsonl").is_some());
}

#[test]
fn trials_are_moved_without_replacing_an_existing_directory() {
    let fs = MemFs::new();
    let journal = seeded(&fs);
    fs.put(format!("{TRIALS}/other"), "x");
    let error = journal
        .archive(stamp(5), ArchiveKind::ResetAll, &[])
        .unwrap_err();
    assert!(matches!(
        error,
        ArchiveError::Dir(DirError::ArchiveExists { .. })
    ));
    assert_eq!(
        fs.contents("/state/trials/0001/sample.csv").unwrap(),
        b"t,v\n"
    );
}

#[test]
fn the_archive_listing_and_ids_see_the_new_archive() {
    let fs = MemFs::new();
    let dir = seeded(&fs)
        .archive(stamp(5), ArchiveKind::ResetAll, &[])
        .unwrap();
    let listed: Vec<SessionId> = dir.archives().unwrap();
    assert_eq!(listed, [default_session()]);
    let next = dir.session_id(crate::testing::stamp(0).time).unwrap();
    assert_eq!(next.to_string(), "20231114T221320Z-2");
    assert!(fs.kind_is_none(&format!("{TRIALS}-x")));
}

fn recorded_but_not_moved(fs: &MemFs) -> Vec<u8> {
    let journal = seeded(fs);
    fs.inject(Fault::before(crate::fs::Op::Rename, 1));
    assert!(
        journal
            .archive(stamp(5), transition("20231115T000000Z"), &[])
            .is_err()
    );
    fs.clear_faults();
    live(fs)
}

#[test]
fn a_recorded_reset_all_still_leaves_the_permanent_boundary() {
    let fs = MemFs::new();
    let recorded = recorded_but_not_moved(&fs);
    let mut dir = simulated(&fs);
    assert_eq!(
        dir.archive_unreadable(ArchiveKind::ResetAll).unwrap(),
        Some(default_session())
    );
    assert_eq!(dir.reset_boundary().unwrap(), Some(default_session()));
    assert_eq!(fs.contents(ARCHIVED).unwrap(), recorded);
    assert_eq!(fs.contents(COMPAT), None);
}

#[test]
fn the_boundary_is_durable_before_a_recorded_reset_all_moves_anything() {
    let fs = MemFs::new();
    let recorded = recorded_but_not_moved(&fs);
    let mut dir = simulated(&fs);
    fs.inject(Fault::before(crate::fs::Op::Open, 1));
    assert!(dir.archive_unreadable(ArchiveKind::ResetAll).is_err());
    fs.clear_faults();
    assert_eq!(live(&fs), recorded);
    assert!(fs.contents(ARCHIVED).is_none());
    assert_eq!(dir.reset_boundary().unwrap(), None);
    assert!(fs.contents("/state/trials/0001/sample.csv").is_some());

    assert_eq!(
        dir.archive_unreadable(ArchiveKind::ResetAll).unwrap(),
        Some(default_session())
    );
    assert_eq!(dir.reset_boundary().unwrap(), Some(default_session()));
    assert_eq!(fs.contents(ARCHIVED).unwrap(), recorded);
}

#[test]
fn a_failed_archive_before_its_event_retries_to_completion() {
    for kind in [ArchiveKind::ResetAll, transition("20231115T000000Z")] {
        let expected = reference(kind);
        for nth in 1..=60 {
            for fault in [Fault::any_before(nth), Fault::any_after(nth)] {
                let fs = MemFs::new();
                let journal = seeded(&fs);
                fs.inject(fault.clone());
                let _ = journal.archive(stamp(5), kind, &[]).map(drop);
                fs.clear_faults();
                let context = format!("{kind:?} {fault:?}");
                let retry = reopen(&fs, 1);
                if fs.contents(ARCHIVED).is_none() {
                    retry
                        .archive(stamp(5), kind, &[])
                        .map(drop)
                        .unwrap_or_else(|error| panic!("{context}: {error}"));
                }
                assert_archived_intact(&fs, &expected, &context);
            }
        }
    }
}

fn undecodable_fixture(fs: &MemFs) -> Vec<u8> {
    unreadable_fixture(fs);
    let text = String::from_utf8(live(fs))
        .unwrap()
        .replace("session.warning", "invented.kind");
    fs.put("/state/events.jsonl", text.clone());
    text.into_bytes()
}

#[test]
fn an_undecodable_journal_archive_retries_through_its_own_markers() {
    for kind in [ArchiveKind::ResetAll, transition("20231115T000000Z")] {
        for nth in 1..=50 {
            for fault in [Fault::any_before(nth), Fault::any_after(nth)] {
                let fs = MemFs::new();
                let original = undecodable_fixture(&fs);
                assert!(
                    simulated(&fs).open(boot(2)).is_err(),
                    "{kind:?}, fault {fault:?}"
                );
                let mut dir = simulated(&fs);
                fs.inject(fault.clone());
                let _ = dir.archive_unreadable(kind);
                fs.clear_faults();
                let context = format!("{kind:?} {fault:?}");
                dir.archive_unreadable(kind)
                    .unwrap_or_else(|error| panic!("{context}: {error}"));
                drop(
                    dir.open(boot(2))
                        .unwrap_or_else(|error| panic!("{context}: {error}")),
                );
                assert_archived_intact(&fs, &original, &context);
                match kind {
                    ArchiveKind::Transition { .. } => {
                        assert!(fs.contents(IMPORT).is_some(), "{context}");
                    }
                    ArchiveKind::ResetAll => assert!(fs.contents(RESET).is_some(), "{context}"),
                }
            }
        }
    }
}

#[test]
fn reopening_after_a_moved_archive_syncs_the_archive_directory_before_the_next_writer() {
    let reference = MemFs::new();
    let journal = seeded(&reference);
    reference.record_history();
    journal
        .archive(stamp(5), ArchiveKind::ResetAll, &[])
        .map(drop)
        .unwrap();
    let log = reference.log();
    let moved = log
        .iter()
        .position(|(op, path)| {
            *op == crate::fs::Op::Rename && path == std::path::Path::new("/state/events.jsonl")
        })
        .unwrap();
    let nth = u64::try_from(moved).unwrap() + 2;

    let fs = MemFs::new();
    let journal = seeded(&fs);
    fs.inject(Fault::any_before(nth));
    assert!(
        journal
            .archive(stamp(5), ArchiveKind::ResetAll, &[])
            .map(drop)
            .is_err()
    );
    fs.clear_faults();
    assert!(fs.contents(ARCHIVED).is_some());
    fs.record_history();
    let journal = reopen(&fs, 2);
    assert!(journal.events().is_empty());
    let log = fs.log();
    let archive_sync = log
        .iter()
        .position(|(op, path)| {
            *op == crate::fs::Op::SyncDir && path == std::path::Path::new("/state/archive")
        })
        .unwrap();
    let writer = log
        .iter()
        .position(|(op, path)| {
            *op == crate::fs::Op::Open && path == std::path::Path::new("/state/events.jsonl")
        })
        .unwrap();
    assert!(archive_sync < writer);
}

fn position(log: &[(Op, std::path::PathBuf)], op: Op, path: &str) -> Option<usize> {
    log.iter()
        .position(|(logged, at)| *logged == op && at == std::path::Path::new(path))
}

#[test]
fn open_syncs_a_surviving_compat_marker_and_its_directories_before_resuming_the_archive() {
    let fs = MemFs::new();
    let original = unreadable_fixture(&fs);
    let mut dir = simulated(&fs);
    fs.inject(Fault::before(Op::Sync, 1).named("20231114T221320Z-compat-pending"));
    assert!(dir.archive_unreadable(ArchiveKind::ResetAll).is_err());
    fs.clear_faults();
    drop(dir);
    assert!(fs.contents(COMPAT).is_some());
    assert_eq!(live(&fs), original);

    fs.record_history();
    let journal = reopen(&fs, 2);
    assert!(journal.events().is_empty());
    assert_archived_intact(&fs, &original, "resumed");
    let log = fs.log();
    let first_removal = position(&log, Op::Remove, "/state/state.json").unwrap();
    for (op, path) in [
        (Op::Sync, COMPAT),
        (Op::SyncDir, "/state/archive"),
        (Op::SyncDir, "/state"),
    ] {
        let synced = position(&log, op, path).unwrap_or_else(|| panic!("{op:?} {path} never ran"));
        assert!(
            synced < first_removal,
            "{op:?} {path} must precede the first destructive step"
        );
    }
}

#[test]
fn a_transition_retry_syncs_the_pending_import_it_keeps_as_its_source() {
    let fs = MemFs::new();
    drop(
        seeded(&fs)
            .archive(stamp(5), transition("20231115T000000Z"), &[])
            .unwrap(),
    );
    let mut second = begin(&fs, session("20231115T000000Z"));
    second.append(stamp(6), note("more"), &[]).unwrap();
    fs.inject(Fault::before(Op::Sync, 1).named(".import-pending.tmp"));
    assert!(
        second
            .archive(stamp(7), transition("20231116T000000Z"), &[])
            .is_err()
    );
    fs.clear_faults();
    assert_eq!(fs.contents("/state/archive/20231115T000000Z.jsonl"), None);
    assert_eq!(fs.contents(IMPORT).unwrap(), b"20231115T000000Z\n");

    fs.record_history();
    let dir = reopen(&fs, 3)
        .archive(stamp(7), transition("20231116T000000Z"), &[])
        .unwrap();
    assert!(position(&fs.log(), Op::Sync, IMPORT_TMP).is_some());
    assert!(position(&fs.log(), Op::Rename, IMPORT_TMP).is_some());
    assert_eq!(
        dir.pending_import().unwrap(),
        Some(ImportMarker {
            source: default_session(),
            destination: session("20231116T000000Z"),
        })
    );
    assert_eq!(dir.markers().unwrap().len(), 1);
    assert_eq!(dir.archives().unwrap().len(), 2);
}

#[test]
fn recovering_an_already_moved_archive_fails_closed_at_every_step_and_retries_once() {
    let data = b"{\"seq\":1}\n";
    for nth in 1..=4 {
        for fault in [Fault::any_before(nth), Fault::any_after(nth)] {
            let fs = MemFs::new();
            fs.put(ARCHIVED, data.as_slice());
            fs.put(COMPAT, "");
            let dir = simulated(&fs);
            fs.inject(fault.clone());
            let context = format!("{fault:?}");
            let error = dir.open(boot(2)).unwrap_err();
            assert!(matches!(error, crate::OpenError::Dir(_)), "{context}");
            assert!(fs.kind_is_none("/state/events.jsonl"), "{context}");
            assert_eq!(fs.contents(ARCHIVED).unwrap(), data, "{context}");
            let removed = nth == 4 || (nth == 3 && fault == Fault::any_after(3));
            assert_eq!(fs.contents(COMPAT).is_none(), removed, "{context}");

            fs.clear_faults();
            let journal = simulated(&fs)
                .open(boot(3))
                .unwrap_or_else(|error| panic!("{context}: {error}"));
            assert!(journal.events().is_empty(), "{context}");
            assert_eq!(fs.contents(COMPAT), None, "{context}");
            assert_eq!(fs.contents(ARCHIVED).unwrap(), data, "{context}");
        }
    }
}
