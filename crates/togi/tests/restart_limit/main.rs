#[path = "../support/mod.rs"]
mod support;

use support::{Host, files};
use togi_cli::Exit;

const GRUBENV: &str = "fake grubenv";
const GET: &str = "get togi_restart_count";

fn run(host: &Host, state: &std::path::Path) -> support::Ran {
    host.run(state, &["restart-limit", "--tuning-boot", GRUBENV])
}

fn count_get() -> String {
    "get togi_restart_count".to_owned()
}

#[test]
fn every_count_picks_its_boot_action_in_order() {
    for (initial, count, retry) in [
        (None, 1, true),
        (Some("0"), 1, true),
        (Some("1"), 2, true),
        (Some("2"), 3, false),
        (Some("3"), 3, false),
    ] {
        let host = Host::new();
        let state = tempfile::tempdir().unwrap();
        *host.count.borrow_mut() = initial.map(str::to_owned);
        host.marker.set(!retry);
        let ran = run(&host, state.path());
        assert_eq!((ran.exit, ran.out.as_str()), (Exit::Ok, ""), "{initial:?}");
        let want: Vec<String> = if retry {
            vec![count_get(), "set".into(), "create marker".into(), "reboot".into()]
        } else {
            vec![
                count_get(),
                "set".into(),
                "clear saved_entry".into(),
                "remove marker".into(),
                "reboot".into(),
            ]
        };
        assert_eq!(host.steps(), want, "{initial:?}");
        assert_eq!(host.marker_at_reboot.get(), Some(retry), "{initial:?}");
        let phrase = if retry {
            "rebooting back into the tuning boot"
        } else {
            "rebooting into the normal system"
        };
        assert_eq!(
            ran.err,
            format!("togi restart-limit: attempt {count} of 3; {phrase}\n"),
            "{initial:?}"
        );
        assert_eq!(host.grubenvs.borrow().as_slice(), [std::path::PathBuf::from(GRUBENV)]);
        assert_eq!(files(state.path()), Vec::<String>::new(), "touched the state directory");
    }
}

#[test]
fn a_failure_stops_at_its_step_and_leaves_the_marker_right() {
    struct Case {
        name: &'static str,
        count: &'static str,
        fail: &'static str,
        marker_before: bool,
        steps: &'static [&'static str],
        marker_after: bool,
    }
    for case in [
        Case { name: "read", count: "0", fail: GET, marker_before: false, steps: &[GET], marker_after: false },
        Case { name: "invalid count", count: "invalid", fail: "", marker_before: false, steps: &[GET], marker_after: false },
        Case { name: "write", count: "0", fail: "set", marker_before: false, steps: &[GET, "set"], marker_after: false },
        Case { name: "clear", count: "2", fail: "clear saved_entry", marker_before: true, steps: &[GET, "set", "clear saved_entry"], marker_after: true },
        Case { name: "create", count: "0", fail: "create marker", marker_before: false, steps: &[GET, "set", "create marker"], marker_after: false },
        Case { name: "exhausted marker cleanup", count: "2", fail: "remove marker", marker_before: true, steps: &[GET, "set", "clear saved_entry", "remove marker"], marker_after: true },
        Case { name: "reboot", count: "0", fail: "reboot", marker_before: false, steps: &[GET, "set", "create marker", "reboot", "remove marker"], marker_after: false },
        Case { name: "exhausted reboot", count: "2", fail: "reboot", marker_before: true, steps: &[GET, "set", "clear saved_entry", "remove marker", "reboot"], marker_after: false },
    ] {
        let host = Host::new();
        let state = tempfile::tempdir().unwrap();
        *host.count.borrow_mut() = Some(case.count.to_owned());
        host.marker.set(case.marker_before);
        host.fail.borrow_mut().extend(Some(case.fail).filter(|fail| !fail.is_empty()));
        let ran = run(&host, state.path());
        assert_eq!(ran.exit, Exit::Error, "{}: {}", case.name, ran.err);
        assert_eq!(ran.out, "", "{}", case.name);
        assert!(!ran.err.is_empty(), "{}", case.name);
        assert_eq!(host.steps(), case.steps, "{}", case.name);
        assert_eq!(host.marker.get(), case.marker_after, "{}", case.name);
        if case.fail == "reboot" {
            assert!(
                ran.err.contains("systemctl reboot exited with exit status: 1: denied"),
                "{}: {}",
                case.name,
                ran.err
            );
        }
    }
}

#[test]
fn a_marker_that_cannot_be_removed_after_a_failed_reboot_is_reported() {
    let host = Host::new();
    let state = tempfile::tempdir().unwrap();
    *host.count.borrow_mut() = Some("0".into());
    host.fail.borrow_mut().extend(["reboot", "remove marker"]);
    let ran = run(&host, state.path());
    assert_eq!(ran.exit, Exit::Error);
    assert!(
        ran.err.contains("systemctl reboot exited with exit status: 1: denied"),
        "{}",
        ran.err
    );
    assert!(
        ran.err.contains("remove retry marker after failed reboot: injected failure"),
        "{}",
        ran.err
    );
    assert!(host.marker.get(), "the failed removal leaves the marker");
}

#[test]
fn usage_errors_and_help_touch_nothing() {
    for (args, exit) in [
        (&["restart-limit", "--help"][..], Exit::Ok),
        (&["restart-limit", "--tuning-boot", "must not open", "--help"], Exit::Ok),
        (&["restart-limit"], Exit::Usage),
        (&["restart-limit", "--tuning-boot="], Exit::Usage),
        (&["restart-limit", "--tuning-boot"], Exit::Usage),
        (&["restart-limit", "--tuning-boot", "unused", "extra"], Exit::Usage),
        (&["restart-limit", "--force"], Exit::Usage),
    ] {
        let host = Host::new();
        let state = tempfile::tempdir().unwrap();
        let ran = host.run(state.path(), args);
        assert_eq!(ran.exit, exit, "{args:?}: {}", ran.err);
        assert_eq!(host.steps(), Vec::<String>::new(), "{args:?}");
        assert!(host.grubenvs.borrow().is_empty(), "{args:?}");
        if exit == Exit::Ok {
            assert_eq!(ran.err, "", "{args:?}");
        } else {
            assert_eq!(ran.out, "", "{args:?}");
            assert!(ran.err.contains("--tuning-boot"), "{args:?}: {}", ran.err);
        }
    }
}

#[test]
fn the_state_directory_flag_is_accepted_after_the_command_and_config_never() {
    for args in [
        &["restart-limit", "--config", "unused.json", "--help"][..],
        &["--config", "unused.json", "restart-limit", "--help"],
    ] {
        let host = Host::new();
        let state = tempfile::tempdir().unwrap();
        let ran = host.run(state.path(), args);
        assert_eq!(ran.exit, Exit::Usage, "{args:?}");
        assert_eq!(ran.out, "");
        assert!(
            ran.err.contains("--config applies only to doctor, run and reset"),
            "{args:?}: {}",
            ran.err
        );
    }
    let host = Host::new();
    let ran = host.run(
        std::path::Path::new("unused"),
        &["restart-limit", "--tuning-boot", GRUBENV, "--state-dir", "other"],
    );
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
}
