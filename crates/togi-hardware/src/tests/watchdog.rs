use std::fs;
use std::path::Path;

use tempfile::TempDir;
use togi_machine::Check;

use crate::watchdog::probe;

fn device(root: &Path, index: usize, state: &str, identity: Option<&str>) {
    let dir = root.join(format!("sys/class/watchdog/watchdog{index}"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("state"), state).unwrap();
    if let Some(identity) = identity {
        fs::write(dir.join("identity"), identity).unwrap();
    }
}

fn ready(detail: &str) -> Check {
    Check {
        name: "watchdog".to_owned(),
        detail: detail.to_owned(),
        ok: true,
    }
}

fn refused(detail: &str) -> Check {
    Check {
        ok: false,
        ..ready(detail)
    }
}

#[test]
fn no_watchdog_state_is_not_ready() {
    let root = TempDir::new().unwrap();
    assert_eq!(
        probe(root.path()),
        refused("no hardware watchdog state available")
    );
}

#[test]
fn a_device_without_a_state_file_is_not_a_candidate() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join("sys/class/watchdog/watchdog0")).unwrap();
    assert_eq!(
        probe(root.path()),
        refused("no hardware watchdog state available")
    );
}

#[test]
fn an_inactive_device_is_not_ready() {
    let root = TempDir::new().unwrap();
    device(root.path(), 0, "inactive\n", Some("SP5100 TCO timer\n"));
    assert_eq!(
        probe(root.path()),
        refused("watchdog0 state \"inactive\", want active")
    );
}

#[test]
fn an_active_hardware_device_is_ready() {
    let root = TempDir::new().unwrap();
    device(root.path(), 0, "active\n", Some("SP5100 TCO timer\n"));
    assert_eq!(
        probe(root.path()),
        ready("watchdog0 (SP5100 TCO timer) active")
    );
}

#[test]
fn the_software_watchdog_is_not_hardware() {
    let root = TempDir::new().unwrap();
    device(root.path(), 0, "active\n", Some("Software Watchdog\n"));
    assert_eq!(
        probe(root.path()),
        refused("watchdog0 is not a hardware watchdog (\"Software Watchdog\")")
    );
}

#[test]
fn an_empty_identity_is_not_hardware() {
    let root = TempDir::new().unwrap();
    device(root.path(), 0, "active\n", Some(""));
    assert_eq!(
        probe(root.path()),
        refused("watchdog0 is not a hardware watchdog (\"\")")
    );
}

#[test]
fn a_missing_identity_is_a_read_error() {
    let root = TempDir::new().unwrap();
    device(root.path(), 0, "active\n", None);
    let check = probe(root.path());
    assert!(!check.ok);
    assert!(
        check.detail.starts_with("read watchdog0 identity:"),
        "{}",
        check.detail
    );
}

#[test]
fn an_unknown_state_is_not_ready() {
    let root = TempDir::new().unwrap();
    device(root.path(), 0, "enabled\n", Some("SP5100 TCO timer\n"));
    assert_eq!(
        probe(root.path()),
        refused("watchdog0 state \"enabled\", want active")
    );
}

#[test]
fn a_later_hardware_device_makes_it_ready() {
    let root = TempDir::new().unwrap();
    device(root.path(), 0, "active\n", Some("Software Watchdog\n"));
    device(root.path(), 1, "active\n", Some("i6300ESB timer\n"));
    assert_eq!(
        probe(root.path()),
        ready("watchdog1 (i6300ESB timer) active")
    );
}

#[test]
fn every_refusal_is_reported_when_none_is_ready() {
    let root = TempDir::new().unwrap();
    device(root.path(), 0, "inactive\n", Some("SP5100 TCO timer\n"));
    device(root.path(), 1, "active\n", Some("Software Watchdog\n"));
    let check = probe(root.path());
    assert!(!check.ok);
    assert_eq!(
        check.detail,
        "watchdog0 state \"inactive\", want active; \
         watchdog1 is not a hardware watchdog (\"Software Watchdog\")"
    );
}

#[test]
fn an_unreadable_state_is_refused() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join("sys/class/watchdog/watchdog0/state")).unwrap();
    let check = probe(root.path());
    assert!(!check.ok);
    assert!(
        check.detail.starts_with("read watchdog0 state:"),
        "{}",
        check.detail
    );
}

#[test]
fn an_unreadable_state_does_not_hide_a_later_hardware_device() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join("sys/class/watchdog/watchdog0/state")).unwrap();
    device(root.path(), 1, "active\n", Some("Test hardware timer\n"));
    assert_eq!(
        probe(root.path()),
        ready("watchdog1 (Test hardware timer) active")
    );
}

#[test]
fn directories_that_are_not_watchdogs_are_ignored() {
    let root = TempDir::new().unwrap();
    let other = root.path().join("sys/class/watchdog/other0");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("state"), "active\n").unwrap();
    fs::write(other.join("identity"), "Not a watchdog\n").unwrap();
    assert_eq!(
        probe(root.path()),
        refused("no hardware watchdog state available")
    );
}

#[test]
fn a_class_directory_that_cannot_be_listed_fails_with_its_cause() {
    let root = TempDir::new().unwrap();
    let class = root.path().join("sys/class/watchdog");
    fs::create_dir_all(class.parent().unwrap()).unwrap();
    fs::write(&class, "").unwrap();
    let check = probe(root.path());
    assert!(!check.ok);
    assert!(
        check.detail.starts_with("find hardware watchdogs: "),
        "{}",
        check.detail
    );
    assert!(check.detail.contains("Not a directory"), "{}", check.detail);
}
