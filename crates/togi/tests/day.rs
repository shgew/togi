//! The day command refuses missing sessions and shares existing writer/hardware exclusion.

#![cfg(test)]

mod support;

use jiff::Timestamp;
use std::path::Path;
use togi_cli::Exit;
use togi_cli::system::System;
use togi_journal::{SessionId, SessionStart, Stamp};

fn seed(host: &support::Host) {
    let dir = host.lock_state_dir(Path::new("/state")).unwrap();
    let mut journal = dir
        .open_at("seed".to_owned().into(), Timestamp::UNIX_EPOCH)
        .unwrap();
    journal
        .append(
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: None,
            },
            SessionStart {
                build: togi_cli::report::current_build(),
                session: SessionId::new(Timestamp::UNIX_EPOCH, None).unwrap(),
                cores: host.topology.clone(),
            }
            .into(),
            &[],
        )
        .unwrap();
    journal.close().unwrap();
}

#[test]
fn no_session_refuses_without_creating_any_state() {
    let host = support::Host::new();
    let ran = host.run(Path::new("/state"), &["day"]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(ran.err.contains("no session journal"), "{}", ran.err);
    assert!(!host.exists("/state"));
    assert_eq!(host.lock_acquisitions.get(), 0);
}

#[test]
fn day_refuses_a_busy_host_before_opening_hardware() {
    let host = support::Host::new();
    seed(&host);
    let _held = host.acquire_host_lock().unwrap();
    let ran = host.run(Path::new("/state"), &["day"]);
    assert_eq!(ran.exit, Exit::Locked);
    assert!(ran.err.contains("lock"), "{}", ran.err);
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn day_refuses_a_busy_journal_before_opening_hardware() {
    let host = support::Host::new();
    seed(&host);
    let _held = host.lock_state_dir(Path::new("/state")).unwrap();
    let ran = host.run(Path::new("/state"), &["day"]);
    assert_eq!(ran.exit, Exit::Locked);
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn status_remains_lock_free_while_day_owns_both_locks() {
    let host = support::Host::new();
    seed(&host);
    let _hardware = host.acquire_host_lock().unwrap();
    let _writer = host.lock_state_dir(Path::new("/state")).unwrap();
    let before = host.lock_acquisitions.get();
    let ran = host.run(Path::new("/state"), &["status"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(host.lock_acquisitions.get(), before);
}

#[test]
fn destination_opens_the_existing_journal_without_starting_another_session() {
    let host = support::Host::new();
    seed(&host);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut context = togi_cli::cli::Context {
        state_dir: Path::new("/state"),
        out: &mut out,
        err: &mut err,
        system: &host,
    };
    let journal = togi_cli::day_destination::open(
        &mut context,
        "day".to_owned().into(),
        &togi_cli::report::current_build(),
    )
    .unwrap();
    assert_eq!(journal.events().len(), 1);
    assert!(journal.session().is_some());
    journal.close().unwrap();
}

#[test_case::test_case(&["run", "--no-tui"] ; "run")]
#[test_case::test_case(&["reset", "--all"] ; "reset")]
fn tuning_writers_cannot_start_while_day_owns_both_locks(args: &[&str]) {
    let host = support::Host::new();
    seed(&host);
    let _hardware = host.acquire_host_lock().unwrap();
    let _writer = host.lock_state_dir(Path::new("/state")).unwrap();
    let ran = host.run(Path::new("/state"), args);
    assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    assert_eq!(host.calls.get(), 0);
}
