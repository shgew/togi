use std::cell::Cell;
use std::path::PathBuf;
use std::time::Duration;

use test_case::test_case;
use togi_machine::{CancellationToken, Check, WatchdogState};

use super::support::Tree;
use crate::watchdog::{Active, Cancelled, Ready, arming_with, wait};

fn device(root: &Tree, index: usize, state: &str, identity: Option<&str>) {
    let dir = format!("sys/class/watchdog/watchdog{index}");
    root.put(&format!("{dir}/state"), state);
    root.put(&format!("{dir}/timeout"), "60\n");
    root.put(&format!("{dir}/timeleft"), "58\n");
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

fn counter(root: &Tree, attribute: &str, sample: Option<Result<&str, ()>>) {
    let path = format!("sys/class/watchdog/watchdog0/{attribute}");
    root.remove(&path);
    match sample {
        Some(Ok(value)) => root.put(&path, value),
        Some(Err(())) => root.mkdir(&path),
        None => {}
    }
}

#[test_case(&[Some(Ok("58\n")), Some(Ok("57\n"))], true; "counting")]
#[test_case(&[Some(Ok("60\n")), Some(Ok("59\n"))], true; "at timeout")]
#[test_case(&[Some(Ok("1\n")), Some(Ok("0\n"))], true; "at expiry")]
#[test_case(&[Some(Ok("4294967295\n"))], false; "all ones after deep sleep")]
#[test_case(&[Some(Ok("61\n"))], false; "larger than timeout")]
#[test_case(&[Some(Err(()))], false; "present but unreadable")]
#[test_case(&[Some(Ok("invalid\n"))], false; "malformed")]
#[test_case(&[Some(Ok("58\n")), Some(Ok("60\n")), Some(Ok("59\n"))], true; "refreshed then counting")]
#[test_case(&[Some(Ok("58\n")), Some(Ok("58\n")), Some(Ok("57\n"))], true; "equal refresh then counting")]
#[test_case(&[Some(Ok("30\n")), Some(Ok("30\n")), Some(Ok("58\n")), Some(Ok("57\n"))], true; "equal then refresh then counting")]
#[test_case(&[Some(Ok("30\n")), Some(Ok("58\n")), Some(Ok("58\n")), Some(Ok("57\n"))], true; "refresh then equal then counting")]
#[test_case(&[Some(Ok("30\n")), Some(Ok("30\n")), Some(Ok("58\n")), Some(Ok("58\n")), Some(Ok("57\n"))], true; "equal refresh equal then counting")]
#[test_case(&[Some(Ok("58\n")), Some(Ok("58\n")), Some(Ok("58\n")), Some(Ok("58\n")), Some(Ok("58\n"))], false; "static across all samples")]
#[test_case(&[Some(Ok("58\n")), Some(Ok("60\n")), Some(Ok("60\n")), Some(Ok("60\n")), Some(Ok("60\n"))], false; "refreshed then static")]
#[test_case(&[Some(Ok("56\n")), Some(Ok("57\n")), Some(Ok("58\n")), Some(Ok("59\n")), Some(Ok("60\n"))], false; "repeated refresh is not countdown")]
#[test_case(&[Some(Ok("30\n")), Some(Ok("30\n")), Some(Ok("58\n")), Some(Ok("58\n")), Some(Ok("58\n"))], false; "middle refresh never decreases")]
#[test_case(&[Some(Ok("58\n")), None], false; "counter disappears")]
#[test_case(&[Some(Ok("58\n")), Some(Err(()))], false; "later sample unreadable")]
#[test_case(&[Some(Ok("58\n")), Some(Ok("invalid\n"))], false; "later sample malformed")]
#[test_case(&[Some(Ok("58\n")), Some(Ok("61\n"))], false; "refresh exceeds timeout")]
#[test_case(&[Some(Ok("58\n")), Some(Ok("60\n")), Some(Err(()))], false; "refreshed sample unreadable")]
#[test_case(&[Some(Ok("30\n")), Some(Ok("30\n")), Some(Ok("58\n")), Some(Err(()))], false; "fourth sample unreadable")]
#[test_case(&[Some(Ok("30\n")), Some(Ok("30\n")), Some(Ok("58\n")), Some(Ok("58\n")), Some(Err(()))], false; "fifth sample unreadable")]
#[test_case(&[Some(Ok("30\n")), Some(Ok("30\n")), Some(Ok("58\n")), Some(Ok("61\n"))], false; "fourth sample exceeds timeout")]
#[test_case(&[Some(Ok("30\n")), Some(Ok("30\n")), Some(Ok("58\n")), Some(Ok("58\n")), Some(Ok("61\n"))], false; "fifth sample exceeds timeout")]
fn counter_readiness_requires_observed_progress(
    samples: &[Option<Result<&str, ()>>],
    counting: bool,
) {
    let root = Tree::empty();
    device(&root, 0, "active\n", Some("SP5100 TCO timer\n"));
    counter(&root, "timeleft", samples[0]);
    let next = Cell::new(1);
    let check = root
        .watchdog_with(|duration| {
            assert_eq!(duration, Duration::from_secs(1));
            counter(&root, "timeleft", samples[next.get()]);
            next.set(next.get() + 1);
            Ok(())
        })
        .unwrap();
    assert_eq!(next.get(), samples.len());
    assert_eq!(check.ok, counting, "{}", check.detail);
    if !counting {
        assert!(
            check.detail.contains("open but not counting"),
            "{}",
            check.detail
        );
        assert!(!arming_with(&Err(check), Ok(held(&["/dev/watchdog0"]))).is_armed());
    }
}

#[test_case(true; "static counter")]
#[test_case(false; "missing counter")]
fn a_counter_without_observed_progress_is_not_ready(present: bool) {
    let root = Tree::empty();
    device(&root, 0, "active\n", Some("SP5100 TCO timer\n"));
    if !present {
        root.remove("sys/class/watchdog/watchdog0/timeleft");
    }
    let check = root.watchdog_with(|_| Ok(())).unwrap();
    assert!(!check.ok, "{}", check.detail);
    assert!(!arming_with(&Err(check), Ok(held(&["/dev/watchdog0"]))).is_armed());
}

#[test_case(None; "missing timeout")]
#[test_case(Some(Err(())); "unreadable timeout")]
#[test_case(Some(Ok("invalid\n")); "malformed timeout")]
fn an_unreadable_timeout_is_not_ready(timeout: Option<Result<&str, ()>>) {
    let root = Tree::empty();
    device(&root, 0, "active\n", Some("SP5100 TCO timer\n"));
    counter(&root, "timeout", timeout);
    let check = root
        .watchdog_with(|_| panic!("invalid timeout must refuse before waiting"))
        .unwrap();
    assert!(!check.ok, "{}", check.detail);
    assert!(
        check
            .detail
            .starts_with("watchdog0 open but not counting: read timeout:"),
        "{}",
        check.detail
    );
    assert!(!arming_with(&Err(check), Ok(held(&["/dev/watchdog0"]))).is_armed());
}

#[test_case(1; "the first wait")]
#[test_case(3; "a middle wait")]
#[test_case(4; "the last wait")]
fn a_cancelled_wait_stops_sampling_and_is_not_a_refusal(cancel_at: usize) {
    let root = Tree::empty();
    device(&root, 0, "active\n", Some("SP5100 TCO timer\n"));
    let waits = Cell::new(0);
    let reads = Cell::new(0);
    let result = root.watchdog_observing(
        |duration| {
            assert_eq!(duration, Duration::from_secs(1));
            waits.set(waits.get() + 1);
            if waits.get() == cancel_at {
                Err(Cancelled)
            } else {
                Ok(())
            }
        },
        |path| {
            if path.file_name().is_some_and(|name| name == "timeleft") {
                reads.set(reads.get() + 1);
            }
        },
    );
    assert_eq!(result, Err(Cancelled));
    assert_eq!(waits.get(), cancel_at);
    // The first sample precedes every wait; no sample follows the cancelled one.
    assert_eq!(reads.get(), cancel_at);
}

#[test_case("inactive\n", "58\n", "watchdog0 state \"inactive\", want active"; "inactive")]
#[test_case("active\n", "4294967295\n", "watchdog0 open but not counting: timeleft 4294967295 s exceeds timeout 60 s"; "all ones after deep sleep")]
fn a_refusal_before_any_wait_stands_when_waits_would_cancel(
    state: &str,
    timeleft: &str,
    detail: &str,
) {
    let root = Tree::empty();
    device(&root, 0, state, Some("SP5100 TCO timer\n"));
    counter(&root, "timeleft", Some(Ok(timeleft)));
    let waits = Cell::new(0);
    let result = root.watchdog_with(|_| {
        waits.set(waits.get() + 1);
        Err(Cancelled)
    });
    assert_eq!(result, Ok(refused(detail)));
    assert_eq!(waits.get(), 0);
}

fn two_hardware_devices() -> Tree {
    let root = Tree::empty();
    device(&root, 0, "active\n", Some("SP5100 TCO timer\n"));
    device(&root, 1, "active\n", Some("i6300ESB timer\n"));
    root
}

#[test]
fn a_cancelled_wait_outranks_an_earlier_ready_device() {
    assert_eq!(
        two_hardware_devices().watchdog(),
        ready("watchdog0 (SP5100 TCO timer) active"),
        "without cancellation both devices count down and the first is reported"
    );
    let root = two_hardware_devices();
    let waits = Cell::new(0);
    let result = root.watchdog_with(|_| {
        waits.set(waits.get() + 1);
        if waits.get() == 1 {
            // watchdog0 counts down in its wait, so it is ready before watchdog1 is sampled.
            counter(&root, "timeleft", Some(Ok("57\n")));
            Ok(())
        } else {
            Err(Cancelled)
        }
    });
    assert_eq!(result, Err(Cancelled));
    assert_eq!(waits.get(), 2);
}

#[test]
fn the_probe_wait_ends_when_its_token_is_cancelled() {
    assert_eq!(wait(&CancellationToken::new(), Duration::ZERO), Ok(()));
    let root = Tree::empty();
    device(&root, 0, "active\n", Some("SP5100 TCO timer\n"));
    let cancel = CancellationToken::new();
    cancel.cancel();
    // A cancelled token returns from the wait at once, so the full interval is never slept.
    assert_eq!(
        root.watchdog_with(|duration| wait(&cancel, duration)),
        Err(Cancelled)
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
