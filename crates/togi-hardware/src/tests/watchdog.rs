use togi_machine::Check;

use super::support::Tree;

fn device(root: &Tree, index: usize, state: &str, identity: Option<&str>) {
    let dir = format!("sys/class/watchdog/watchdog{index}");
    root.put(&format!("{dir}/state"), state);
    if let Some(identity) = identity {
        root.put(&format!("{dir}/identity"), identity);
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
    let root = Tree::empty();
    assert_eq!(
        root.watchdog(),
        refused("no hardware watchdog state available")
    );
}

#[test]
fn a_device_without_a_state_file_is_not_a_candidate() {
    let root = Tree::empty();
    root.mkdir("sys/class/watchdog/watchdog0");
    assert_eq!(
        root.watchdog(),
        refused("no hardware watchdog state available")
    );
}

#[test]
fn an_inactive_device_is_not_ready() {
    let root = Tree::empty();
    device(&root, 0, "inactive\n", Some("SP5100 TCO timer\n"));
    assert_eq!(
        root.watchdog(),
        refused("watchdog0 state \"inactive\", want active")
    );
}

#[test]
fn an_active_hardware_device_is_ready() {
    let root = Tree::empty();
    device(&root, 0, "active\n", Some("SP5100 TCO timer\n"));
    assert_eq!(
        root.watchdog(),
        ready("watchdog0 (SP5100 TCO timer) active")
    );
}

#[test]
fn the_software_watchdog_is_not_hardware() {
    let root = Tree::empty();
    device(&root, 0, "active\n", Some("Software Watchdog\n"));
    assert_eq!(
        root.watchdog(),
        refused("watchdog0 is not a hardware watchdog (\"Software Watchdog\")")
    );
}

#[test]
fn an_empty_identity_is_not_hardware() {
    let root = Tree::empty();
    device(&root, 0, "active\n", Some(""));
    assert_eq!(
        root.watchdog(),
        refused("watchdog0 is not a hardware watchdog (\"\")")
    );
}

#[test]
fn a_missing_identity_is_a_read_error() {
    let root = Tree::empty();
    device(&root, 0, "active\n", None);
    let check = root.watchdog();
    assert!(!check.ok);
    assert!(
        check.detail.starts_with("read watchdog0 identity:"),
        "{}",
        check.detail
    );
}

#[test]
fn an_unknown_state_is_not_ready() {
    let root = Tree::empty();
    device(&root, 0, "enabled\n", Some("SP5100 TCO timer\n"));
    assert_eq!(
        root.watchdog(),
        refused("watchdog0 state \"enabled\", want active")
    );
}

#[test]
fn a_later_hardware_device_makes_it_ready() {
    let root = Tree::empty();
    device(&root, 0, "active\n", Some("Software Watchdog\n"));
    device(&root, 1, "active\n", Some("i6300ESB timer\n"));
    assert_eq!(root.watchdog(), ready("watchdog1 (i6300ESB timer) active"));
}

#[test]
fn every_refusal_is_reported_when_none_is_ready() {
    let root = Tree::empty();
    device(&root, 0, "inactive\n", Some("SP5100 TCO timer\n"));
    device(&root, 1, "active\n", Some("Software Watchdog\n"));
    let check = root.watchdog();
    assert!(!check.ok);
    assert_eq!(
        check.detail,
        "watchdog0 state \"inactive\", want active; \
         watchdog1 is not a hardware watchdog (\"Software Watchdog\")"
    );
}

#[test]
fn an_unreadable_state_is_refused() {
    let root = Tree::empty();
    root.mkdir("sys/class/watchdog/watchdog0/state");
    let check = root.watchdog();
    assert!(!check.ok);
    assert!(
        check.detail.starts_with("read watchdog0 state:"),
        "{}",
        check.detail
    );
}

#[test]
fn an_unreadable_state_does_not_hide_a_later_hardware_device() {
    let root = Tree::empty();
    root.mkdir("sys/class/watchdog/watchdog0/state");
    device(&root, 1, "active\n", Some("Test hardware timer\n"));
    assert_eq!(
        root.watchdog(),
        ready("watchdog1 (Test hardware timer) active")
    );
}

#[test]
fn directories_that_are_not_watchdogs_are_ignored() {
    let root = Tree::empty();
    root.put("sys/class/watchdog/other0/state", "active\n");
    root.put("sys/class/watchdog/other0/identity", "Not a watchdog\n");
    assert_eq!(
        root.watchdog(),
        refused("no hardware watchdog state available")
    );
}

#[test]
fn a_class_directory_that_cannot_be_listed_fails_with_its_cause() {
    let root = Tree::empty();
    root.put("sys/class/watchdog", "");
    let check = root.watchdog();
    assert!(!check.ok);
    assert!(
        check.detail.starts_with("find hardware watchdogs: "),
        "{}",
        check.detail
    );
    assert!(check.detail.contains("Not a directory"), "{}", check.detail);
}
