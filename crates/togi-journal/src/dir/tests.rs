use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::*;
use crate::fs::{Fault, MemFs, Op};

fn dir(fs: &MemFs) -> StateDir {
    crate::testing::simulated(fs)
}

fn sid(text: &str) -> SessionId {
    text.parse().unwrap()
}

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

fn id_at(second: i64) -> SessionId {
    SessionId::new(at(second), None).unwrap()
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Doc {
    n: u32,
}

#[test]
fn a_second_writer_is_refused_until_the_first_drops() {
    let fs = MemFs::new();
    let first = dir(&fs);
    assert!(matches!(
        crate::testing::try_simulated(&fs),
        Err(LockError::Held { .. })
    ));
    drop(first);
    assert!(crate::testing::try_simulated(&fs).is_ok());
}

#[test]
fn locking_on_disk_creates_every_missing_directory() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("a/b/c");
    let held = StateDir::lock(&path).unwrap();
    assert!(path.join("lock").is_file());
    assert!(matches!(StateDir::lock(&path), Err(LockError::Held { .. })));
    drop(held);
    assert!(StateDir::lock(&path).is_ok());
}

#[test]
fn locking_refuses_a_state_path_that_is_a_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    std::fs::write(&path, "x").unwrap();
    assert!(matches!(
        StateDir::lock(path.join("state")),
        Err(LockError::Dir(DirError::NotADirectory { .. }))
    ));
}

#[test]
fn state_round_trips_and_a_missing_one_reads_as_none() {
    let fs = MemFs::new();
    let mut dir = dir(&fs);
    assert_eq!(dir.read_state::<Doc>().unwrap(), None);
    dir.write_state(&Doc { n: 7 }).unwrap();
    assert_eq!(dir.read_state::<Doc>().unwrap(), Some(Doc { n: 7 }));
}

#[test]
fn a_damaged_state_is_an_error_not_an_absence() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    fs.put("/state/state.json", "{\"n\":");
    assert!(matches!(
        dir.read_state::<Doc>(),
        Err(StateError::Decode(_))
    ));
}

#[test]
fn state_publication_leaves_the_old_or_the_new_document_under_every_fault() {
    let faults = [Op::Open, Op::Write, Op::Sync, Op::Rename, Op::SyncDir]
        .into_iter()
        .flat_map(|op| [Fault::before(op, 1), Fault::after(op, 1)])
        .chain([Fault::short_write(1)]);
    for fault in faults {
        let fs = MemFs::new();
        let mut dir = dir(&fs);
        dir.write_state(&Doc { n: 1 }).unwrap();
        fs.inject(fault.clone());
        let failed = dir.write_state(&Doc { n: 2 }).is_err();
        assert!(failed, "{fault:?} should fail the publication");
        let read = dir.read_state::<Doc>().unwrap().unwrap();
        assert!(
            read == Doc { n: 1 } || read == Doc { n: 2 },
            "{fault:?} left {read:?}"
        );
        fs.clear_faults();
        dir.write_state(&Doc { n: 3 }).unwrap();
        assert_eq!(dir.read_state::<Doc>().unwrap(), Some(Doc { n: 3 }));
    }
}

#[test]
fn a_failed_publication_never_replaces_the_state_with_a_partial_write() {
    let fs = MemFs::new();
    let mut dir = dir(&fs);
    dir.write_state(&Doc { n: 1 }).unwrap();
    fs.fail_state_publication();
    assert!(dir.write_state(&Doc { n: 2 }).is_err());
    assert_eq!(dir.read_state::<Doc>().unwrap(), Some(Doc { n: 1 }));
}

#[test]
fn the_directory_is_synced_after_the_state_rename() {
    let fs = MemFs::new();
    let mut dir = dir(&fs);
    dir.write_state(&Doc { n: 1 }).unwrap();
    let log = fs.log();
    let rename = log.iter().rposition(|(op, _)| *op == Op::Rename).unwrap();
    assert_eq!(log[rename + 1].0, Op::SyncDir);
    let sync = log[..rename]
        .iter()
        .rposition(|(op, _)| *op == Op::Sync)
        .unwrap();
    assert!(sync < rename);
}

#[test]
fn archives_list_regular_journals_in_numeric_session_order() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    for name in [
        "20260101T000000Z-10",
        "20260101T000000Z-2",
        "20260101T000000Z",
        "20250101T000000Z-3",
    ] {
        fs.put(format!("/state/archive/{name}.jsonl"), "");
    }
    fs.put("/state/archive/notes.jsonl", "");
    fs.put("/state/archive/20260101T000000Z-trials/x", "");
    fs.symlink("/state/archive/x", "/state/archive/20270101T000000Z.jsonl");
    let listed: Vec<String> = dir
        .archives()
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        listed,
        [
            "20250101T000000Z-3",
            "20260101T000000Z",
            "20260101T000000Z-2",
            "20260101T000000Z-10"
        ]
    );
}

#[test]
fn markers_list_by_session_and_kind_and_ignore_other_names() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    fs.put("/state/archive/20260101T000000Z-2-reset-all", "");
    fs.put("/state/archive/20260101T000000Z-carry-pending", "");
    fs.put("/state/archive/stray-carry-pending", "");
    assert_eq!(
        dir.markers().unwrap(),
        [
            (Marker::CarryPending, sid("20260101T000000Z")),
            (Marker::ResetAll, sid("20260101T000000Z-2")),
        ]
    );
}

#[test]
fn a_marker_that_is_not_a_regular_file_is_refused() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    fs.put("/state/archive/20260101T000000Z-reset-all/inside", "");
    assert!(matches!(dir.markers(), Err(DirError::NotRegular { .. })));
    assert!(matches!(
        dir.reset_boundary(),
        Err(DirError::NotRegular { .. })
    ));
}

#[test]
fn a_symlinked_reset_marker_is_never_followed() {
    let fs = MemFs::new();
    let mut dir = dir(&fs);
    fs.put("/state/archive/20260101T000000Z.jsonl", "");
    fs.put("/elsewhere", "");
    fs.symlink("/elsewhere", "/state/archive/20260101T000000Z-reset-all");
    assert!(matches!(
        dir.mark_reset_all(),
        Err(DirError::NotRegular { .. })
    ));
    assert_eq!(fs.contents("/elsewhere").unwrap(), b"");
}

#[test]
fn pending_markers_clear_but_the_reset_boundary_is_permanent() {
    let fs = MemFs::new();
    let mut dir = dir(&fs);
    fs.put("/state/archive/20260101T000000Z-carry-pending", "");
    fs.put("/state/archive/20260102T000000Z-carry-pending", "");
    fs.put("/state/archive/20260101T000000Z-reset-all", "");
    assert_eq!(dir.pending_carry().unwrap(), Some(sid("20260102T000000Z")));
    dir.clear_pending(Marker::CarryPending).unwrap();
    assert_eq!(dir.pending_carry().unwrap(), None);
    assert!(matches!(
        dir.clear_pending(Marker::ResetAll),
        Err(DirError::PermanentMarker(Marker::ResetAll))
    ));
    assert_eq!(dir.reset_boundary().unwrap(), Some(sid("20260101T000000Z")));
}

#[test]
fn a_directory_with_no_session_gets_no_reset_boundary() {
    let fs = MemFs::new();
    let mut dir = dir(&fs);
    assert_eq!(dir.mark_reset_all().unwrap(), None);
    assert_eq!(dir.reset_boundary().unwrap(), None);
    assert!(
        fs.entries()
            .iter()
            .all(|(path, _)| !path.starts_with("/state/archive"))
    );
}

#[test]
fn the_reset_boundary_is_the_newest_source_there_is() {
    let fs = MemFs::new();
    let mut dir = dir(&fs);
    fs.put("/state/archive/20260101T000000Z.jsonl", "");
    fs.put("/state/archive/20260301T000000Z-carry-pending", "");
    fs.put("/state/archive/20260201T000000Z-compat-pending", "");
    assert_eq!(dir.mark_reset_all().unwrap(), Some(sid("20260301T000000Z")));
    assert_eq!(dir.reset_boundary().unwrap(), Some(sid("20260301T000000Z")));
}

fn carry_pending_dir(fs: &MemFs) -> StateDir {
    fs.put("/state/archive/20260101T000000Z.jsonl", "");
    fs.put("/state/archive/20260101T000000Z-carry-pending", "");
    dir(fs)
}

#[test]
fn the_reset_boundary_is_synced_before_a_carry_marker_is_removed() {
    let fs = MemFs::new();
    let mut dir = carry_pending_dir(&fs);
    dir.drop_pending_carry().unwrap();
    let log = fs.log();
    let boundary = "/state/archive/20260101T000000Z-reset-all";
    let carry = "/state/archive/20260101T000000Z-carry-pending";
    let created = log
        .iter()
        .position(|(op, path)| *op == Op::Open && path == Path::new(boundary))
        .unwrap();
    let synced = log
        .iter()
        .position(|(op, path)| *op == Op::Sync && path == Path::new(boundary))
        .unwrap();
    let dirs: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(index, (op, _))| *op == Op::SyncDir && *index > synced)
        .map(|(index, _)| index)
        .collect();
    let removed = log
        .iter()
        .position(|(op, path)| *op == Op::Remove && path == Path::new(carry))
        .unwrap();
    assert!(created < synced);
    assert!(
        dirs.len() >= 3,
        "archive and state directory sync before and after the removal"
    );
    assert!(dirs[0] < removed && dirs[1] < removed);
    assert!(dirs[2] > removed);
}

#[test]
fn a_fault_anywhere_in_dropping_a_carry_never_loses_the_boundary_or_the_carry() {
    for nth in 1..=12 {
        for fault in [Fault::any_before(nth), Fault::any_after(nth)] {
            let fs = MemFs::new();
            let mut dir = carry_pending_dir(&fs);
            fs.inject(fault.clone());
            let result = dir.drop_pending_carry();
            fs.clear_faults();
            let carry = dir.pending_carry().unwrap();
            let boundary = dir.reset_boundary().unwrap();
            if carry.is_none() {
                assert_eq!(boundary, Some(sid("20260101T000000Z")), "{fault:?}");
            }
            if result.is_err() {
                dir.drop_pending_carry().unwrap();
            }
            assert_eq!(dir.pending_carry().unwrap(), None, "{fault:?}");
            assert_eq!(
                dir.reset_boundary().unwrap(),
                Some(sid("20260101T000000Z")),
                "{fault:?}"
            );
        }
    }
}

#[test]
fn dropping_without_a_pending_carry_invents_no_boundary() {
    let fs = MemFs::new();
    fs.put("/state/archive/20260101T000000Z.jsonl", "");
    let mut dir = dir(&fs);
    assert_eq!(dir.drop_pending_carry().unwrap(), None);
    assert_eq!(dir.reset_boundary().unwrap(), None);
}

#[test]
fn a_session_id_is_the_utc_second_until_it_is_taken() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    assert_eq!(
        dir.session_id(at(1_700_000_000)).unwrap().to_string(),
        "20231114T221320Z"
    );
    fs.put("/state/archive/20231114T221320Z.jsonl", "");
    assert_eq!(
        dir.session_id(at(1_700_000_000)).unwrap().to_string(),
        "20231114T221320Z-2"
    );
}

#[test]
fn archived_trial_directories_and_markers_reserve_an_id() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    fs.put("/state/archive/20231114T221320Z-trials/x", "");
    fs.put("/state/archive/20231114T221320Z-2-carry-pending", "");
    assert_eq!(
        dir.session_id(at(1_700_000_000)).unwrap().to_string(),
        "20231114T221320Z-3"
    );
}

#[test]
fn a_dangling_symlink_reserves_an_id() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    fs.symlink("/nowhere", "/state/archive/20231114T221320Z.jsonl");
    assert_eq!(
        dir.session_id(at(1_700_000_000)).unwrap().to_string(),
        "20231114T221320Z-2"
    );
}

#[test]
fn suffixes_order_numerically_so_ten_follows_nine() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    for suffix in 2..=10 {
        fs.put(
            format!("/state/archive/20231114T221320Z-{suffix}.jsonl"),
            "",
        );
    }
    fs.put("/state/archive/20231114T221320Z.jsonl", "");
    assert_eq!(
        dir.session_id(at(1_700_000_000)).unwrap().to_string(),
        "20231114T221320Z-11"
    );
}

#[test]
fn an_id_always_follows_the_reset_boundary_whatever_the_clock_says() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    let boundary = id_at(1_700_000_000);
    fs.put(format!("/state/archive/{boundary}-reset-all"), "");
    for earlier in [1_700_000_000, 1_600_000_000, 0] {
        let id = dir.session_id(at(earlier)).unwrap();
        assert!(id > boundary, "{id} must follow {boundary}");
        assert_eq!(id.timestamp(), boundary.timestamp());
    }
    assert!(dir.session_id(at(1_700_000_001)).unwrap() > boundary);
}

#[test]
fn an_id_follows_a_suffixed_boundary_after_deleted_archives() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    fs.put("/state/archive/20231114T221320Z-10-reset-all", "");
    assert_eq!(
        dir.session_id(at(1_700_000_000)).unwrap().to_string(),
        "20231114T221320Z-11"
    );
}

#[test]
fn an_id_at_the_largest_suffix_is_exhausted_not_wrapped() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    fs.put(
        format!("/state/archive/20231114T221320Z-{}-reset-all", u32::MAX),
        "",
    );
    assert!(matches!(
        dir.session_id(at(1_700_000_000)),
        Err(DirError::SessionIdsExhausted { .. })
    ));
}

#[test]
fn only_a_live_session_start_names_the_live_session() {
    let fs = MemFs::new();
    let dir = dir(&fs);
    assert_eq!(dir.live_session().unwrap(), None);
    fs.put(
        "/state/events.jsonl",
        "{\"seq\":1,\"kind\":\"session.start\",\"data\":{\"session\":\"20231114T221320Z\"}}",
    );
    assert_eq!(dir.live_session().unwrap(), None);
    fs.put(
        "/state/events.jsonl",
        "{\"seq\":1,\"kind\":\"session.start\",\"data\":{\"session\":\"20231114T221320Z\"}}\n",
    );
    assert_eq!(dir.live_session().unwrap(), Some(sid("20231114T221320Z")));
    fs.put(
        "/state/events.jsonl",
        "{\"seq\":1,\"kind\":\"mce\",\"data\":{}}\n",
    );
    assert!(matches!(
        dir.live_session(),
        Err(DirError::NotAJournal { .. })
    ));
}

#[test]
fn every_lock_syncs_the_whole_created_chain_again_after_a_failed_sync() {
    let fs = MemFs::new();
    let path = Path::new("/a/b/c");
    fs.inject(Fault::before(Op::SyncDir, 1));
    assert!(StateDir::lock_in(Arc::new(fs.clone()), path, false).is_err());
    fs.clear_faults();
    let mark = fs.log().len();
    let held = StateDir::lock_in(Arc::new(fs.clone()), path, false).unwrap();
    let log = fs.log();
    let synced: Vec<&Path> = log[mark..]
        .iter()
        .filter(|(op, _)| *op == Op::SyncDir)
        .map(|(_, path)| path.as_path())
        .collect();
    drop(held);
    for expected in ["/", "/a", "/a/b"] {
        assert!(
            synced.contains(&Path::new(expected)),
            "{expected} not synced in {synced:?}"
        );
    }
}

#[test]
fn a_failed_lock_never_leaks_the_lock() {
    let fs = MemFs::new();
    fs.inject(Fault::after(Op::Open, 1).named("lock"));
    assert!(crate::testing::try_simulated(&fs).is_err());
    fs.clear_faults();
    assert!(crate::testing::try_simulated(&fs).is_ok());
}

#[test]
fn a_failed_mode_change_takes_no_lock() {
    let fs = MemFs::new();
    fs.inject(Fault::before(Op::SetMode, 1));
    assert!(crate::testing::try_simulated(&fs).is_err());
    assert_eq!(fs.contents("/state/lock"), None);
    fs.clear_faults();
    assert!(crate::testing::try_simulated(&fs).is_ok());
}

#[test]
fn the_directory_mode_is_set_before_the_lock_file_is_opened() {
    let fs = MemFs::new();
    let _held = dir(&fs);
    let log = fs.log();
    let mode = log
        .iter()
        .position(|(op, path)| *op == Op::SetMode && path == Path::new("/state"))
        .unwrap();
    let lock = log
        .iter()
        .position(|(op, path)| *op == Op::Open && path == Path::new("/state/lock"))
        .unwrap();
    assert!(mode < lock);
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn locking_an_existing_directory_restores_the_state_mode_before_taking_the_lock() {
    let root = tempfile::tempdir().unwrap();
    for mode in [0o777, 0o700] {
        let path = root.path().join(format!("state-{mode:o}"));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let held = StateDir::lock(&path).unwrap();
        assert_eq!(mode_of(&path), 0o711);
        drop(held);
    }
}

#[test]
fn locking_a_directory_the_owner_cannot_write_still_succeeds() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
    let held = StateDir::lock(&path).unwrap();
    assert_eq!(mode_of(&path), 0o711);
    assert!(path.join("lock").is_file());
    drop(held);
}

#[test]
fn a_symlinked_archive_directory_is_refused_on_disk() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("20260101T000000Z.jsonl"), "").unwrap();
    std::fs::write(outside.join("20260101T000000Z-reset-all"), "").unwrap();
    let path = root.path().join("state");
    let held = StateDir::lock(&path).unwrap();
    std::os::unix::fs::symlink(&outside, path.join("archive")).unwrap();
    assert!(matches!(held.archives(), Err(DirError::Io { .. })));
    assert!(matches!(held.markers(), Err(DirError::Io { .. })));
}

#[test]
fn a_stale_state_temporary_is_replaced_by_a_fresh_file() {
    let fs = MemFs::new();
    let mut dir = dir(&fs);
    fs.put("/state/.state.json.tmp", "stale stale stale stale stale");
    let stale = fs.open_append(Path::new("/state/.state.json.tmp")).unwrap();
    dir.write_state(&Doc { n: 1 }).unwrap();
    drop(stale);
    assert_eq!(dir.read_state::<Doc>().unwrap(), Some(Doc { n: 1 }));
    assert_eq!(fs.contents("/state/.state.json.tmp"), None);
}

#[test]
fn publication_never_writes_through_a_pre_existing_temporary_inode() {
    let fs = MemFs::new();
    let mut dir = dir(&fs);
    dir.write_state(&Doc { n: 1 }).unwrap();
    fs.put("/state/.state.json.tmp", "stale");
    let mut stale = fs.open_append(Path::new("/state/.state.json.tmp")).unwrap();
    fs.fail_state_publication();
    assert!(dir.write_state(&Doc { n: 2 }).is_err());
    stale.write_all(b"tamper").unwrap();
    assert_eq!(dir.read_state::<Doc>().unwrap(), Some(Doc { n: 1 }));
}

#[test]
fn publication_leaves_a_hard_link_to_the_published_state_untouched_on_disk() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state");
    let mut dir = StateDir::lock(&path).unwrap();
    dir.write_state(&Doc { n: 1 }).unwrap();
    let keep = root.path().join("keep");
    std::fs::hard_link(path.join("state.json"), &keep).unwrap();
    std::fs::hard_link(path.join("state.json"), path.join(".state.json.tmp")).unwrap();
    dir.write_state(&Doc { n: 2 }).unwrap();
    assert_eq!(std::fs::read(&keep).unwrap(), b"{\"n\":1}\n");
    assert_eq!(dir.read_state::<Doc>().unwrap(), Some(Doc { n: 2 }));
}
