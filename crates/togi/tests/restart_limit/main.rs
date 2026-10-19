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

#[test_case::test_case(None, 1, true; "absent")]
#[test_case::test_case(Some("0"), 1, true; "zero")]
#[test_case::test_case(Some("1"), 2, true; "one")]
#[test_case::test_case(Some("2"), 3, false; "two")]
#[test_case::test_case(Some("3"), 3, false; "three")]
fn every_count_picks_its_boot_action_in_order(initial: Option<&str>, count: u8, retry: bool) {
    let host = Host::new();
    let state = support::MemoryDir::new();
    *host.count.borrow_mut() = initial.map(str::to_owned);
    host.marker.set(!retry);
    let ran = run(&host, state.path());
    assert_eq!((ran.exit, ran.out.as_str()), (Exit::Ok, ""), "{initial:?}");
    let want: Vec<String> = if retry {
        vec![
            count_get(),
            "set".into(),
            "create marker".into(),
            "reboot".into(),
        ]
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
    assert_eq!(
        host.grubenvs.borrow().as_slice(),
        [std::path::PathBuf::from(GRUBENV)]
    );
    assert_eq!(
        files(&host.fs, state.path()),
        Vec::<String>::new(),
        "touched the state directory"
    );
}

#[test_case::test_case("0", GET, false, &[GET], false; "read")]
#[test_case::test_case("invalid", "", false, &[GET], false; "invalid count")]
#[test_case::test_case("0", "set", false, &[GET, "set"], false; "write")]
#[test_case::test_case("2", "clear saved_entry", true, &[GET, "set", "clear saved_entry"], true; "clear")]
#[test_case::test_case("0", "create marker", false, &[GET, "set", "create marker"], false; "create")]
#[test_case::test_case("2", "remove marker", true, &[GET, "set", "clear saved_entry", "remove marker"], true; "exhausted marker cleanup")]
#[test_case::test_case("0", "reboot", false, &[GET, "set", "create marker", "reboot", "remove marker"], false; "reboot")]
#[test_case::test_case("2", "reboot", true, &[GET, "set", "clear saved_entry", "remove marker", "reboot"], false; "exhausted reboot")]
fn a_failure_stops_at_its_step_and_leaves_the_marker_right(
    count: &str,
    failed_step: &'static str,
    marker_before: bool,
    steps: &[&str],
    marker_after: bool,
) {
    let host = Host::new();
    let state = support::MemoryDir::new();
    *host.count.borrow_mut() = Some(count.to_owned());
    host.marker.set(marker_before);
    host.fail
        .borrow_mut()
        .extend(Some(failed_step).filter(|fail| !fail.is_empty()));
    let ran = run(&host, state.path());
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(ran.out, "");
    assert!(!ran.err.is_empty());
    assert_eq!(host.steps(), steps);
    assert_eq!(host.marker.get(), marker_after);
    for (fail, expected) in [
        (
            "create marker",
            "create retry marker: open /run/togi-retry-tuning-boot: injected failure",
        ),
        (
            "remove marker",
            "remove retry marker: remove /run/togi-retry-tuning-boot: injected failure",
        ),
    ] {
        if failed_step == fail {
            assert!(ran.err.contains(expected), "{}", ran.err);
        }
    }
    if failed_step == "reboot" {
        assert!(
            ran.err.contains("systemctl reboot: exit status 1: denied"),
            "{}",
            ran.err
        );
    }
}

#[test]
fn a_marker_that_cannot_be_removed_after_a_failed_reboot_is_reported() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    *host.count.borrow_mut() = Some("0".into());
    host.fail.borrow_mut().extend(["reboot", "remove marker"]);
    let ran = run(&host, state.path());
    assert_eq!(ran.exit, Exit::Error);
    assert!(
        ran.err.contains("systemctl reboot: exit status 1: denied"),
        "{}",
        ran.err
    );
    assert!(
        ran.err
            .contains("remove retry marker after failed reboot: remove /run/togi-retry-tuning-boot: injected failure"),
        "{}",
        ran.err
    );
    assert!(host.marker.get(), "the failed removal leaves the marker");
}

#[test]
fn a_reboot_that_cannot_run_reports_the_cause_and_removes_the_retry_marker() {
    let host = Host::new();
    let state = support::MemoryDir::new();
    *host.count.borrow_mut() = Some("0".into());
    *host.reboot_error.borrow_mut() = Some(togi_hardware::RebootError::Run(
        togi_command::Error::TimedOut {
            program: "systemctl".into(),
            timeout: std::time::Duration::from_secs(30),
        },
    ));
    let ran = run(&host, state.path());
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(ran.out, "");
    assert!(
        ran.err.ends_with(
            "togi restart-limit: systemctl reboot: run systemctl reboot: systemctl did not finish within 30s\n"
        ),
        "{}",
        ran.err
    );
    assert!(!host.marker.get(), "the retry marker was left behind");
    assert_eq!(
        host.steps().last().map(String::as_str),
        Some("remove marker")
    );
}

#[test_case::test_case(&["restart-limit", "--help"], Exit::Ok; "help")]
#[test_case::test_case(&["restart-limit", "--tuning-boot", "must not open", "--help"], Exit::Ok; "boot help")]
#[test_case::test_case(&["restart-limit"], Exit::Usage; "absent boot")]
#[test_case::test_case(&["restart-limit", "--tuning-boot="], Exit::Usage; "empty boot")]
#[test_case::test_case(&["restart-limit", "--tuning-boot"], Exit::Usage; "missing boot value")]
#[test_case::test_case(&["restart-limit", "--tuning-boot", "unused", "extra"], Exit::Usage; "extra positional")]
#[test_case::test_case(&["restart-limit", "--force"], Exit::Usage; "force")]
fn usage_errors_and_help_touch_nothing(args: &[&str], exit: Exit) {
    let host = Host::new();
    let state = support::MemoryDir::new();
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

#[test_case::test_case(&["restart-limit", "--config", "unused.json", "--help"]; "trailing")]
#[test_case::test_case(&["--config", "unused.json", "restart-limit", "--help"]; "leading")]
fn config_is_never_accepted(args: &[&str]) {
    let host = Host::new();
    let state = support::MemoryDir::new();
    let ran = host.run(state.path(), args);
    assert_eq!(ran.exit, Exit::Usage, "{args:?}");
    assert_eq!(ran.out, "");
    assert!(
        ran.err
            .contains("--config applies only to doctor, run and reset"),
        "{args:?}: {}",
        ran.err
    );
}

#[test]
fn the_state_directory_flag_is_accepted_after_the_command() {
    let host = Host::new();
    let ran = host.run(
        std::path::Path::new("unused"),
        &[
            "restart-limit",
            "--tuning-boot",
            GRUBENV,
            "--state-dir",
            "other",
        ],
    );
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
}
