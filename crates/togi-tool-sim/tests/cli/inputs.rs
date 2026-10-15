use std::path::Path;

use super::common::{Host, sim};

#[test]
fn invalid_arguments_are_usage_errors_that_touch_nothing() {
    let mut host = Host::default();
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
        let ran = host.run(args.iter().copied().chain(["--state-dir", "/state"]));
        assert_eq!(ran.code, Some(2), "{name}: {}", ran.stderr);
        assert!(ran.stderr.contains(named), "{name}: {}", ran.stderr);
        assert!(ran.stdout.is_empty());
        assert!(host.states.is_empty(), "{name}");
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
    let mut host = Host::default();
    host.put("/machine.json", r#"{"unknown_machine_key":1}"#);
    let ran = host.run(["--machine", "/machine.json", "--state-dir", "/state"]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(
        ran.stderr.starts_with("sim: load machine:"),
        "{}",
        ran.stderr
    );
    assert!(host.states.is_empty());
}

#[test]
fn replaying_facts_needs_a_machine_before_any_state_changes() {
    let mut host = Host::default();
    let ran = host.run(["--replay-facts", "--state-dir", "/state"]);
    assert_eq!(ran.code, Some(1));
    assert_eq!(
        ran.stderr,
        "sim: replaying facts needs a machine file naming a facts extract; the default machine has none\n"
    );
    assert!(host.states.is_empty());
}

#[test]
fn a_machine_without_an_extract_cannot_replay() {
    let mut host = Host::default();
    host.put(
        "/machine.json",
        include_str!("../../../../tools/bench/machines/flat-hazard.json"),
    );
    let ran = host.run([
        "--replay-facts",
        "--machine",
        "/machine.json",
        "--state-dir",
        "/state",
    ]);
    assert_eq!(ran.code, Some(1));
    assert!(ran.stderr.starts_with("sim: "));
    assert!(host.states.is_empty());
}

#[test]
fn invalid_input_creates_no_temporary_state() {
    let mut host = Host::default();
    host.put("/machine.json", r#"{"unknown_machine_key":1}"#);
    for args in [vec!["--machine", "/machine.json"], vec!["--replay-facts"]] {
        let ran = host.run(args);
        assert_eq!(ran.code, Some(1), "{}", ran.stderr);
        assert!(!ran.stderr.contains("state directory"));
    }
    assert!(host.states.is_empty());
    assert!(host.files.contains_key(Path::new("/machine.json")));
}

#[test]
fn overlays_are_loaded_through_the_same_memory_source() {
    let mut host = Host::default();
    host.put(
        "/target-shared-voltage.json",
        include_str!("../../../../tools/bench/machines/target-shared-voltage.json"),
    );
    host.put(
        "/overlay.json",
        include_str!("../../../../tools/bench/machines/target-r7-vf-boost.json"),
    );
    let ran = host.run([
        "--machine",
        "/overlay.json",
        "--state-dir",
        "/state",
        "--max-boots",
        "1",
    ]);
    assert!(matches!(ran.code, Some(0 | 3)), "{}", ran.stderr);
    assert!(!host.events("/state").is_empty());
}
