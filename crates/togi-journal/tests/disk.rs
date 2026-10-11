use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;

use jiff::Timestamp;
use togi_journal::{
    ArchiveKind, Build, Journal, LockError, Payload, Recording, SCHEMA, Seq, SessionId,
    SessionStart, SessionWarning, SmuCommand, Stamp, StateDir,
};
use togi_machine::{BootId, TargetOffset};

const CHILD: &str = "child_appends_then_aborts";
const ENV: &str = "TOGI_JOURNAL_ABORT_DIR";
const APPENDS: u64 = 25;

#[cfg(test)]
mod support {
    use super::*;

    pub(crate) fn stamp(second: i64) -> Stamp {
        Stamp {
            time: Timestamp::from_second(1_700_000_000 + second).unwrap(),
            mono: None,
        }
    }

    pub(crate) fn boot(n: u32) -> BootId {
        BootId::from(format!("boot-{n}"))
    }

    pub(crate) fn start() -> Payload {
        Payload::SessionStart(SessionStart {
            build: Build {
                version: "0.1.0".into(),
                rev: None,
                ruleset: 1,
                schema: SCHEMA,
                fixes: 0,
                evidence: 1,
            },
            session: "20231114T221320Z".parse::<SessionId>().unwrap(),
            cores: Vec::new(),
        })
    }

    pub(crate) fn note(text: &str) -> Payload {
        Payload::SessionWarning(SessionWarning {
            operation: text.into(),
            trial: None,
            error: String::new(),
        })
    }

    pub(crate) fn begin(dir: StateDir) -> Journal {
        let mut journal = dir.open(boot(1)).unwrap();
        journal.append(stamp(0), start(), &[]).unwrap();
        journal
    }

    pub(crate) fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    pub(crate) fn current_umask() -> u32 {
        let probe = tempfile::tempdir().unwrap();
        let path = probe.path().join("probe");
        std::fs::create_dir(&path).unwrap();
        0o777 & !mode(&path)
    }

    pub(crate) fn appended(journal: &Journal) -> Vec<(Seq, Payload)> {
        journal
            .events()
            .iter()
            .map(|event| (event.seq, event.payload.clone()))
            .collect()
    }

    pub(crate) fn on_disk(path: &Path) -> Vec<(Seq, Payload)> {
        Recording::read(path)
            .unwrap()
            .events()
            .iter()
            .map(|event| (event.seq, event.payload.clone()))
            .collect()
    }
}

use support::{appended, begin, boot, mode, note, on_disk, stamp};

#[test]
fn a_disk_journal_survives_close_reopen_state_and_archive() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("nested/state");
    let mut journal = begin(StateDir::lock(&state).unwrap());
    journal.append(stamp(1), note("one"), &[]).unwrap();
    journal.write_state(&[1, 2, 3]).unwrap();
    assert!(matches!(
        StateDir::lock(&state),
        Err(LockError::Held { .. })
    ));
    let dir = journal.close().unwrap();
    drop(dir);

    let journal = StateDir::lock(&state).unwrap().open(boot(2)).unwrap();
    assert_eq!(journal.events().len(), 2);
    assert_eq!(
        journal.read_state::<Vec<i32>>().unwrap(),
        Some(vec![1, 2, 3])
    );
    assert_eq!(
        Recording::read(&state.join("events.jsonl"))
            .unwrap()
            .events()
            .len(),
        2
    );

    std::fs::create_dir_all(state.join("trials/0001")).unwrap();
    let dir = journal
        .archive(stamp(2), ArchiveKind::ResetAll, &[])
        .unwrap();
    assert!(!state.join("events.jsonl").exists());
    assert!(!state.join("state.json").exists());
    assert!(state.join("archive/20231114T221320Z.jsonl").is_file());
    assert!(state.join("archive/20231114T221320Z-trials/0001").is_dir());
    assert!(state.join("archive/20231114T221320Z-reset-all").is_file());
    assert_eq!(
        dir.reset_boundary().unwrap(),
        Some("20231114T221320Z".parse().unwrap())
    );
}

#[test]
fn a_symlinked_journal_is_never_followed() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    drop(StateDir::lock(&state).unwrap());
    let elsewhere = root.path().join("elsewhere.jsonl");
    std::fs::write(&elsewhere, "").unwrap();
    std::os::unix::fs::symlink(&elsewhere, state.join("events.jsonl")).unwrap();
    assert!(StateDir::lock(&state).unwrap().open(boot(1)).is_err());
    assert_eq!(std::fs::read(&elsewhere).unwrap(), b"");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "pins the unsynced buffered mode the simulator uses"
)]
fn the_unsynced_mode_buffers_until_flush_close_or_64_kib() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let path = state.join("events.jsonl");
    let mut journal = begin(StateDir::lock_unsynced(&state).unwrap());
    journal.append(stamp(1), note("small"), &[]).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"");
    journal.flush().unwrap();
    assert_eq!(on_disk(&path), appended(&journal));
    assert_eq!(on_disk(&path).len(), 2);

    journal.append(stamp(2), note("buffered"), &[]).unwrap();
    assert_eq!(on_disk(&path), appended(&journal)[..2]);
    journal
        .append(stamp(3), note(&"x".repeat(70 * 1024)), &[])
        .unwrap();
    assert_eq!(on_disk(&path), appended(&journal));
    assert_eq!(on_disk(&path).len(), 4);

    journal.append(stamp(4), note("tail"), &[]).unwrap();
    let expected = appended(&journal);
    assert_eq!(on_disk(&path), expected[..4]);
    let dir = journal.close().unwrap();
    drop(dir);
    assert_eq!(on_disk(&path), expected);
    assert_eq!(expected.len(), 5);
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "pins the unsynced buffered mode the simulator uses"
)]
fn a_buffered_journal_dropped_without_close_loses_only_what_was_buffered() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let mut journal = begin(StateDir::lock_unsynced(&state).unwrap());
    journal.append(stamp(1), note("flushed"), &[]).unwrap();
    journal.flush().unwrap();
    let flushed = appended(&journal);
    assert_eq!(flushed[1].1, note("flushed"));
    journal.append(stamp(2), note("lost"), &[]).unwrap();
    drop(journal);
    assert_eq!(on_disk(&state.join("events.jsonl")), flushed);
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "pins the unsynced buffered mode the simulator uses"
)]
fn an_unsynced_smu_intent_receipt_is_minted_only_after_the_intent_reached_the_file() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let path = state.join("events.jsonl");
    let mut journal = begin(StateDir::lock_unsynced(&state).unwrap());
    journal.append(stamp(1), note("before"), &[]).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"");

    let receipt = journal
        .append_smu_intent(
            stamp(2),
            SmuCommand::SetAll {
                offset: TargetOffset::ZERO,
            },
            &[],
        )
        .unwrap();
    let written = on_disk(&path);
    assert_eq!(written, appended(&journal));
    assert_eq!(written.len(), 3);
    let (seq, payload) = written.last().unwrap();
    assert_eq!(*seq, receipt.intent());
    assert!(matches!(payload, Payload::SmuIntent(_)));
}

#[test]
fn child_appends_then_aborts() {
    let Some(dir) = std::env::var_os(ENV) else {
        return;
    };
    let mut journal = begin(StateDir::lock(Path::new(&dir)).unwrap());
    for index in 0..APPENDS {
        journal
            .append(stamp(1), note(&format!("event {index}")), &[])
            .unwrap();
    }
    std::process::abort();
}

#[test]
fn real_process_an_abrupt_exit_loses_no_acknowledged_event() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture", "--test-threads=1"])
        .env(ENV, &state)
        .status()
        .unwrap();
    assert_eq!(
        status.signal(),
        Some(6),
        "the child must die by abort, not exit: {status:?}"
    );

    let journal = StateDir::lock(&state).unwrap().open(boot(2)).unwrap();
    assert_eq!(journal.events().len() as u64, APPENDS + 1);
    assert_eq!(journal.repaired(), None);
}

#[test]
fn the_state_directory_is_traversable_whatever_it_was_before() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let fresh = root.path().join("fresh/state");
    drop(StateDir::lock(&fresh).unwrap());
    assert_eq!(mode(&fresh), 0o711);

    let existing = root.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(StateDir::lock(&existing).unwrap());
    assert_eq!(mode(&existing), 0o711);
    assert_eq!(
        mode(&root.path().join("fresh")),
        0o755 & !support::current_umask()
    );
}

#[test]
fn a_symlinked_state_directory_is_refused_and_its_target_left_alone() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(StateDir::lock(&link).is_err());
    assert_eq!(mode(&target), 0o700);
}
