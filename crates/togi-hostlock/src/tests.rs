use std::fs::{self, File, Permissions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Barrier;
use std::thread;

use nix::sys::stat::Mode as NixMode;
use nix::unistd::mkfifo;
use rustix::process::geteuid;
use tempfile::TempDir;

use super::*;

const HOLD_ENV: &str = "TOGI_HOSTLOCK_TEST_HOLD";
const READY: &str = "togi-hostlock-ready";

/// A temporary directory that passes the containing-directory check under any
/// umask: `tempfile` creates it `0o777 & !umask`, group-writable under 0002.
fn private_dir() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    set_mode(dir.path(), 0o700);
    dir
}

fn lock_in(dir: &TempDir) -> PathBuf {
    dir.path().join("togi.lock")
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, Permissions::from_mode(mode)).unwrap();
}

fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

fn identity(path: &Path) -> (u64, u64) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (metadata.dev(), metadata.ino())
}

fn acquire(path: &Path) -> Result<HostLock, Error> {
    HostLock::acquire(path, Policy::Private)
}

fn legacy_lock(dir: &TempDir, contents: &str, mode: u32) -> PathBuf {
    let path = lock_in(dir);
    fs::write(&path, contents).unwrap();
    set_mode(&path, mode);
    path
}

#[test]
fn real_fs_exclusive_until_dropped() {
    let dir = private_dir();
    let path = lock_in(&dir);
    let lock = acquire(&path).unwrap();
    assert_eq!(lock.path(), path);
    assert_eq!(mode_of(&path), 0o600);
    let before = identity(&path);
    thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let refused = acquire(&path).unwrap_err();
                assert!(matches!(refused, Error::Busy { .. }), "{refused}");
                assert!(refused.to_string().contains(path.to_str().unwrap()));
            });
        }
    });
    drop(lock);
    drop(acquire(&path).unwrap());
    assert_eq!(identity(&path), before);
}

#[test]
fn real_fs_legacy_locks_are_tightened_in_place() {
    for mode in [0o644, 0o444, 0o640] {
        let dir = private_dir();
        let path = legacy_lock(&dir, "persistent host lock\n", mode);
        let before = identity(&path);
        drop(acquire(&path).unwrap());
        assert_eq!(identity(&path), before, "{mode:o}");
        assert_eq!(mode_of(&path), 0o600, "{mode:o}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "persistent host lock\n");
    }
}

#[test]
fn real_fs_legacy_read_only_holder_stays_exclusive_through_migration() {
    let dir = private_dir();
    let path = legacy_lock(&dir, "", 0o644);
    let holder = File::open(&path).unwrap();
    holder.try_lock().unwrap();
    let before = identity(&path);
    for _ in 0..2 {
        let refused = acquire(&path).unwrap_err();
        assert!(matches!(refused, Error::Busy { .. }), "{refused}");
    }
    assert_eq!(identity(&path), before);
    assert_eq!(mode_of(&path), 0o600);
    drop(holder);
    drop(acquire(&path).unwrap());
}

#[test]
fn real_fs_provisioned_group_lock_keeps_its_mode() {
    let dir = private_dir();
    let path = legacy_lock(&dir, "", 0o660);
    drop(acquire(&path).unwrap());
    assert_eq!(mode_of(&path), 0o660);
}

#[test]
fn real_fs_concurrent_creators_yield_one_owner() {
    const CONTENDERS: usize = 8;
    let dir = private_dir();
    let path = lock_in(&dir);
    let start = Barrier::new(CONTENDERS);
    let outcomes: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = (0..CONTENDERS)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    acquire(&path)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    let owners = outcomes.iter().filter(|outcome| outcome.is_ok()).count();
    assert_eq!(owners, 1);
    for refused in outcomes.iter().filter_map(|outcome| outcome.as_ref().err()) {
        assert!(matches!(refused, Error::Busy { .. }), "{refused}");
    }
}

#[test]
fn real_fs_unsafe_lock_objects_are_refused_untouched() {
    for kind in ["symlink", "hardlink", "directory", "fifo"] {
        let dir = private_dir();
        let target = dir.path().join("target");
        fs::write(&target, "unchanged").unwrap();
        set_mode(&target, 0o644);
        let path = lock_in(&dir);
        match kind {
            "symlink" => std::os::unix::fs::symlink(&target, &path).unwrap(),
            "hardlink" => fs::hard_link(&target, &path).unwrap(),
            "directory" => fs::create_dir(&path).unwrap(),
            _ => mkfifo(&path, NixMode::S_IRUSR | NixMode::S_IWUSR).unwrap(),
        }
        let before = (identity(&path), mode_of(&path));
        let refused = acquire(&path).unwrap_err();
        assert!(
            matches!(
                refused,
                Error::UnsafeLock {
                    problem: LockProblem::NotRegularFile | LockProblem::MultipleLinks,
                    ..
                }
            ),
            "{kind}: {refused}"
        );
        assert_eq!((identity(&path), mode_of(&path)), before, "{kind}");
        assert_eq!(mode_of(&target), 0o644, "{kind}");
        assert_eq!(fs::read_to_string(&target).unwrap(), "unchanged", "{kind}");
    }
}

#[test]
fn real_fs_hardlinked_lock_is_named_as_multiply_linked() {
    let dir = private_dir();
    let target = dir.path().join("target");
    fs::write(&target, "").unwrap();
    let path = lock_in(&dir);
    fs::hard_link(&target, &path).unwrap();
    assert!(matches!(
        acquire(&path).unwrap_err(),
        Error::UnsafeLock {
            problem: LockProblem::MultipleLinks,
            ..
        }
    ));
}

#[test]
fn real_fs_writable_ancestor_needs_the_sticky_bit() {
    for sticky in [false, true] {
        let dir = private_dir();
        set_mode(dir.path(), if sticky { 0o1777 } else { 0o777 });
        let child = dir.path().join("child");
        fs::create_dir(&child).unwrap();
        set_mode(&child, 0o700);
        let path = child.join("togi.lock");
        let outcome = acquire(&path);
        if sticky {
            drop(outcome.unwrap());
        } else {
            let refused = outcome.unwrap_err();
            assert!(
                matches!(
                    refused,
                    Error::UnsafeDirectory {
                        problem: DirectoryProblem::WritableWithoutSticky,
                        ..
                    }
                ),
                "{refused}"
            );
            assert!(!path.exists());
        }
    }
}

#[test]
fn real_fs_missing_parent_is_not_created() {
    let dir = private_dir();
    let missing = dir.path().join("missing");
    let path = missing.join("togi.lock");
    let refused = acquire(&path).unwrap_err();
    match &refused {
        Error::Io {
            operation: Operation::ResolveParent,
            source,
            ..
        } => assert_eq!(source.kind(), io::ErrorKind::NotFound),
        other => panic!("unexpected {other}"),
    }
    assert!(refused.to_string().contains(path.to_str().unwrap()));
    assert!(!missing.exists());
}

#[test]
fn real_fs_read_only_descriptor_to_a_secure_lock_is_refused_without_chmod() {
    for mode in [0o600, 0o660] {
        let dir = private_dir();
        let path = legacy_lock(&dir, "persistent", mode);
        let file = File::open(&path).unwrap();
        let refused = secure_lock(&file, &path, Policy::Private, Access::ReadOnly).unwrap_err();
        assert!(
            matches!(
                refused,
                Error::UnsafeLock {
                    problem: LockProblem::ReadOnly,
                    ..
                }
            ),
            "{refused}"
        );
        assert_eq!(mode_of(&path), mode);
    }
}

#[test]
fn real_fs_create_permission_failure_is_reported_as_create() {
    if geteuid().is_root() {
        return;
    }
    let dir = private_dir();
    let read_only = dir.path().join("read-only");
    fs::create_dir(&read_only).unwrap();
    set_mode(&read_only, 0o500);
    let path = read_only.join("togi.lock");
    let outcome = acquire(&path);
    let created = path.exists();
    set_mode(&read_only, 0o700);
    let refused = outcome.unwrap_err();
    match &refused {
        Error::Io {
            operation: Operation::Create,
            source,
            ..
        } => assert_eq!(source.kind(), io::ErrorKind::PermissionDenied),
        other => panic!("unexpected {other}"),
    }
    assert!(refused.to_string().contains(path.to_str().unwrap()));
    assert!(!created);
}

#[test]
fn real_fs_unopenable_lock_is_refused_untouched() {
    if geteuid().is_root() {
        return;
    }
    let dir = private_dir();
    let path = legacy_lock(&dir, "persistent", 0o000);
    let before = identity(&path);
    let refused = acquire(&path).unwrap_err();
    match &refused {
        Error::Io {
            operation: Operation::Open,
            source,
            ..
        } => assert_eq!(source.kind(), io::ErrorKind::PermissionDenied),
        other => panic!("unexpected {other}"),
    }
    assert!(refused.to_string().contains(path.to_str().unwrap()));
    assert_eq!(identity(&path), before);
    assert_eq!(mode_of(&path), 0o000);
    set_mode(&path, 0o600);
    assert_eq!(fs::read_to_string(&path).unwrap(), "persistent");
}

#[test]
fn real_fs_host_policy_leaves_provisioning_to_root() {
    if geteuid().is_root() {
        return;
    }
    let dir = private_dir();
    let path = lock_in(&dir);
    let refused = HostLock::acquire(&path, Policy::Host).unwrap_err();
    assert!(matches!(refused, Error::NotProvisioned { .. }), "{refused}");
    assert!(!path.exists());

    drop(acquire(&path).unwrap());
    let refused = HostLock::acquire(&path, Policy::Host).unwrap_err();
    assert!(
        matches!(
            refused,
            Error::UnsafeLock {
                problem: LockProblem::ForeignOwner,
                ..
            }
        ),
        "{refused}"
    );
}

#[test]
fn real_process_excludes_until_it_exits() {
    let dir = private_dir();
    let path = lock_in(&dir);
    #[expect(
        clippy::disallowed_methods,
        reason = "test helper re-executes the test binary"
    )]
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::real_process_hold_lock_helper",
            "--nocapture",
        ])
        .env(HOLD_ENV, &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut output = BufReader::new(stdout).lines();
    let ready = output
        .by_ref()
        .map(Result::unwrap)
        .any(|line| line == READY);
    assert!(ready, "helper exited before taking the lock");
    let refused = acquire(&path).unwrap_err();
    assert!(matches!(refused, Error::Busy { .. }), "{refused}");
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    drop(acquire(&path).unwrap());
}

#[test]
fn real_process_hold_lock_helper() {
    let Some(path) = std::env::var_os(HOLD_ENV) else {
        return;
    };
    let _lock = acquire(Path::new(&path)).unwrap();
    writeln!(std::io::stdout(), "{READY}").unwrap();
    std::io::stdout().flush().unwrap();
    let mut sink = Vec::new();
    std::io::stdin().read_to_end(&mut sink).unwrap();
}
