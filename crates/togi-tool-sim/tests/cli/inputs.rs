use std::fs;

use crate::common::{entries, sim, sim_in_tmp};

#[test]
fn invalid_arguments_are_usage_errors_that_touch_nothing() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let state = state.to_str().unwrap();
    for (name, args, named) in [
        ("unknown flag", vec!["--unknown"], "--unknown"),
        ("positional argument", vec!["extra"], "extra"),
        ("zero cycles", vec!["--cycles", "0"], "--cycles"),
        ("negative cycles", vec!["--cycles", "-1"], "-1"),
        ("zero boots", vec!["--max-boots", "0"], "--max-boots"),
        (
            "negative verify interval",
            vec!["--verify-every", "-1"],
            "-1",
        ),
        ("non-numeric seed", vec!["--seed", "x"], "--seed"),
    ] {
        let ran = sim(args.iter().copied().chain(["--state-dir", state]));
        assert_eq!(ran.code, Some(2), "{name}: {}", ran.stderr);
        assert!(ran.stderr.contains(named), "{name}: {}", ran.stderr);
        assert!(ran.stdout.is_empty(), "{name}: {}", ran.stdout);
        assert!(!root.path().join("state").exists(), "{name}");
    }
}

#[test]
fn help_is_written_to_standard_error_and_succeeds() {
    let ran = sim(["--help"]);
    assert_eq!(ran.code, Some(0));
    assert!(ran.stdout.is_empty());
    assert!(ran.stderr.contains("--max-boots"), "{}", ran.stderr);
}

#[test]
fn an_invalid_machine_leaves_the_state_directory_untouched() {
    let root = tempfile::tempdir().unwrap();
    let machine = root.path().join("machine.json");
    fs::write(&machine, r#"{"unknown_machine_key":1}"#).unwrap();
    let state = root.path().join("state");
    let ran = sim([
        "--machine".as_ref(),
        machine.as_os_str(),
        "--state-dir".as_ref(),
        state.as_os_str(),
    ]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(
        ran.stderr.starts_with("sim: load machine:"),
        "{}",
        ran.stderr
    );
    assert!(!state.exists());
}

#[test]
fn replaying_facts_needs_a_machine_before_any_state_changes() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let ran = sim([
        "--replay-facts".as_ref(),
        "--state-dir".as_ref(),
        state.as_os_str(),
    ]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert_eq!(
        ran.stderr,
        "sim: replaying facts needs a machine file naming a facts extract; the default machine has none\n"
    );
    assert!(!state.exists());
}

#[test]
fn a_machine_without_an_extract_cannot_replay() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let machine = crate::common::machines().join("flat-hazard.json");
    let ran = sim([
        "--replay-facts".as_ref(),
        "--machine".as_ref(),
        machine.as_os_str(),
        "--state-dir".as_ref(),
        state.as_os_str(),
    ]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(ran.stderr.starts_with("sim: "), "{}", ran.stderr);
    assert!(!state.exists());
}

#[test]
fn invalid_input_creates_no_temporary_state() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let machine = root.path().join("machine.json");
    fs::write(&machine, r#"{"unknown_machine_key":1}"#).unwrap();
    for args in [
        vec!["--machine".as_ref(), machine.as_os_str()],
        vec!["--replay-facts".as_ref()],
    ] {
        let ran = sim_in_tmp(tmp.path(), &args);
        assert_eq!(ran.code, Some(1), "{args:?}: {}", ran.stderr);
        assert!(!ran.stderr.contains("state directory"), "{}", ran.stderr);
    }
    assert!(entries(tmp.path()).is_empty());
}
