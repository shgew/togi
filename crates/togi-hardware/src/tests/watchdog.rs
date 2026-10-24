use std::path::PathBuf;

use test_case::test_case;
use togi_machine::{Check, WatchdogState};

use super::support::Tree;
use crate::watchdog::{Active, Ready, arming_with};

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

fn held(paths: &[&str]) -> Vec<PathBuf> {
    paths.iter().map(PathBuf::from).collect()
}

fn armed(detail: &str) -> WatchdogState {
    WatchdogState::Armed {
        detail: detail.to_owned(),
    }
}

fn unarmed(detail: &str) -> WatchdogState {
    WatchdogState::Unarmed {
        detail: detail.to_owned(),
    }
}

fn ready_device(name: &str, identity: &str) -> Ready {
    Ready {
        name: name.to_owned(),
        identity: identity.to_owned(),
    }
}

fn active(name: &str, identity: &str) -> Active {
    Active {
        first: ready_device(name, identity),
        rest: Vec::new(),
    }
}

fn two_active() -> Active {
    Active {
        first: ready_device("watchdog0", "SP5100 TCO timer"),
        rest: vec![ready_device("watchdog1", "i6300ESB timer")],
    }
}

#[test_case(&["/dev/watchdog1"]; "the second device")]
#[test_case(&["/dev/watchdog0", "/dev/watchdog1"]; "both devices")]
fn pid_1_holding_any_active_hardware_watchdog_arms_it(descriptors: &[&str]) {
    assert!(arming_with(&Ok(two_active()), Ok(held(descriptors))).is_armed());
}

#[test]
fn the_armed_detail_names_the_device_pid_1_holds() {
    assert_eq!(
        arming_with(&Ok(two_active()), Ok(held(&["/dev/watchdog1"]))),
        armed("watchdog1 (i6300ESB timer) active, held by PID 1")
    );
}

#[test]
fn pid_1_holding_neither_of_two_active_watchdogs_is_not_armed() {
    assert_eq!(
        arming_with(
            &Ok(two_active()),
            Ok(held(&["/dev/null", "/dev/watchdog2"]))
        ),
        unarmed(
            "watchdog0 (SP5100 TCO timer) active, watchdog1 (i6300ESB timer) active, but PID 1 does not hold /dev/watchdog0 or /dev/watchdog1"
        )
    );
}

#[test_case("/dev/watchdog0"; "a numbered device")]
#[test_case("/dev/watchdog"; "the compatibility node")]
fn an_active_watchdog_that_pid_1_holds_is_armed(node: &str) {
    assert_eq!(
        arming_with(
            &Ok(active("watchdog0", "SP5100 TCO timer")),
            Ok(held(&["/dev/null", node, "socket:[12]"]))
        ),
        armed("watchdog0 (SP5100 TCO timer) active, held by PID 1")
    );
}

#[test]
fn the_compatibility_node_belongs_to_the_first_watchdog_only() {
    assert_eq!(
        arming_with(
            &Ok(active("watchdog1", "i6300ESB timer")),
            Ok(held(&["/dev/watchdog"]))
        ),
        unarmed("watchdog1 (i6300ESB timer) active, but PID 1 does not hold /dev/watchdog1")
    );
}

#[test_case(&["/dev/watchdog1"]; "a software watchdog it feeds instead")]
#[test_case(&["/dev/null", "/dev/ttyS0"]; "no watchdog at all")]
#[test_case(&["/dev/watchdog00"]; "a name that only starts like the device")]
fn an_active_watchdog_that_pid_1_does_not_hold_is_not_armed(descriptors: &[&str]) {
    assert_eq!(
        arming_with(
            &Ok(active("watchdog0", "SP5100 TCO timer")),
            Ok(held(descriptors))
        ),
        unarmed("watchdog0 (SP5100 TCO timer) active, but PID 1 does not hold /dev/watchdog0")
    );
}

#[test]
fn a_present_but_inactive_watchdog_is_not_armed_whatever_pid_1_holds() {
    assert_eq!(
        arming_with(
            &Err(refused("watchdog0 state \"inactive\", want active")),
            Ok(held(&["/dev/watchdog0"]))
        ),
        unarmed("watchdog0 state \"inactive\", want active")
    );
}

#[test]
fn unreadable_pid_1_descriptors_leave_the_watchdog_unarmed_with_the_cause() {
    let state = arming_with(
        &Ok(active("watchdog0", "SP5100 TCO timer")),
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
    );
    assert!(!state.is_armed());
    assert!(
        state
            .detail()
            .starts_with("watchdog0 (SP5100 TCO timer) active; read PID 1 descriptors: "),
        "{}",
        state.detail()
    );
}
