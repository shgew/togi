use jiff::Timestamp;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use togi_journal::fs::{FileKind, Fs, MemFs};
use togi_journal::{
    ArchiveKind, Build, FixStamp, Journal, LockError, Payload, SCHEMA, SessionId, SessionStart,
    SessionWarning, Stamp, StateDir,
};
use togi_machine::BootId;

const CHILD: &str = "real_process_child_appends_then_is_killed";
const ENV: &str = "TOGI_JOURNAL_KILL_DIR";
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
                fixes: FixStamp::none(),
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
}

use support::{begin, boot, mode, note, stamp};

#[expect(
    clippy::unwrap_used,
    reason = "a kernel fixture must acquire its unsynced state directory"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "kernel and process tests explicitly use the non-durable disk adapter"
)]
fn unsynced(path: &Path) -> StateDir {
    StateDir::lock_with_fs(togi_journal::fs::unsynced_disk_fs(), path).unwrap()
}

#[test]
fn a_journal_survives_close_reopen_state_and_archive() {
    let fs = MemFs::new();
    let state = Path::new("/nested/state");
    #[expect(
        clippy::disallowed_methods,
        reason = "this lifecycle test runs entirely in memory"
    )]
    let lock = || StateDir::lock_with_fs(Arc::new(fs.clone()), state);
    let mut journal = begin(lock().unwrap());
    journal.append(stamp(1), note("one"), &[]).unwrap();
    journal.write_state(&[1, 2, 3]).unwrap();
    assert!(matches!(lock(), Err(LockError::Held { .. })));
    let dir = journal.close().unwrap();
    drop(dir);

    let journal = lock().unwrap().open(boot(2)).unwrap();
    assert_eq!(journal.events().len(), 2);
    assert_eq!(
        journal.read_state::<Vec<i32>>().unwrap(),
        Some(vec![1, 2, 3])
    );
    assert_eq!(
        togi_journal::Recording::read_in(&fs, &state.join("events.jsonl"))
            .unwrap()
            .events()
            .len(),
        2
    );

    fs.create_dir(&state.join("trials")).unwrap();
    fs.create_dir(&state.join("trials/0001")).unwrap();
    let dir = journal
        .archive(stamp(2), ArchiveKind::ResetAll, &[])
        .unwrap();
    assert_eq!(fs.kind(&state.join("events.jsonl")).unwrap(), None);
    assert_eq!(fs.kind(&state.join("state.json")).unwrap(), None);
    assert_eq!(
        fs.kind(&state.join("archive/20231114T221320Z.jsonl"))
            .unwrap(),
        Some(FileKind::File)
    );
    assert_eq!(
        fs.kind(&state.join("archive/20231114T221320Z-trials/0001"))
            .unwrap(),
        Some(FileKind::Dir)
    );
    assert_eq!(
        fs.kind(&state.join("archive/20231114T221320Z-reset-all"))
            .unwrap(),
        Some(FileKind::File)
    );
    assert_eq!(
        dir.reset_boundary().unwrap(),
        Some("20231114T221320Z".parse().unwrap())
    );
}

#[test]
fn real_fs_a_symlinked_journal_is_never_followed() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    drop(unsynced(&state));
    let elsewhere = root.path().join("elsewhere.jsonl");
    std::fs::write(&elsewhere, "").unwrap();
    std::os::unix::fs::symlink(&elsewhere, state.join("events.jsonl")).unwrap();
    assert!(unsynced(&state).open(boot(1)).is_err());
    assert_eq!(std::fs::read(&elsewhere).unwrap(), b"");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "pins the in-memory simulation tripwire without disk I/O"
)]
fn a_simulated_journal_reads_back_whole() {
    let fs = togi_journal::fs::MemFs::new();
    let mut journal = begin(StateDir::simulated(fs.clone()).unwrap());
    journal.append(stamp(1), note("simulated"), &[]).unwrap();
    let expected: Vec<_> = journal
        .events()
        .iter()
        .map(|event| (event.seq, event.payload.clone()))
        .collect();
    drop(journal.close().unwrap());
    let written = togi_journal::Recording::read_in(&fs, Path::new("/state/events.jsonl")).unwrap();
    let actual: Vec<_> = written
        .events()
        .iter()
        .map(|event| (event.seq, event.payload.clone()))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn real_process_child_appends_then_is_killed() {
    let Some(dir) = std::env::var_os(ENV) else {
        return;
    };
    let mut journal = begin(unsynced(Path::new(&dir)));
    for index in 0..APPENDS {
        journal
            .append(stamp(1), note(&format!("event {index}")), &[])
            .unwrap();
    }
    // SIGKILL rather than abort: equally abrupt, but it never dumps core.
    rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL)
        .unwrap();
    unreachable!("SIGKILL cannot be caught");
}

#[test]
fn real_process_an_abrupt_exit_loses_no_acknowledged_event() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    #[expect(
        clippy::disallowed_methods,
        reason = "test helper re-executes the test binary"
    )]
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture", "--test-threads=1"])
        .env(ENV, &state)
        .status()
        .unwrap();
    assert_eq!(
        status.signal(),
        Some(rustix::process::Signal::KILL.as_raw()),
        "the child must die by SIGKILL, not exit: {status:?}"
    );

    let journal = unsynced(&state).open(boot(2)).unwrap();
    assert_eq!(journal.events().len() as u64, APPENDS + 1);
    assert_eq!(journal.repaired(), None);
}

#[test]
fn real_fs_the_state_directory_is_traversable_whatever_it_was_before() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let fresh = root.path().join("fresh/state");
    drop(unsynced(&fresh));
    assert_eq!(mode(&fresh), 0o711);

    let existing = root.path().join("existing");
    std::fs::create_dir(&existing).unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(unsynced(&existing));
    assert_eq!(mode(&existing), 0o711);
    assert_eq!(
        mode(&root.path().join("fresh")),
        0o755 & !support::current_umask()
    );
}

#[test]
fn real_fs_a_symlinked_state_directory_is_refused_and_its_target_left_alone() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "kernel refusal must use the non-durable disk adapter"
    )]
    let result = StateDir::lock_with_fs(togi_journal::fs::unsynced_disk_fs(), &link);
    assert!(result.is_err());
    assert_eq!(mode(&target), 0o700);
}
