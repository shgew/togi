use std::path::Path;

use test_case::test_case;

use super::common::{Host, sim};

#[test_case(&["--unknown"], "--unknown"; "unknown flag")]
#[test_case(&["extra"], "extra"; "positional argument")]
#[test_case(&["--cycles", "0"], "--cycles"; "zero cycles")]
#[test_case(&["--cycles", "-1"], "-1"; "negative cycles")]
#[test_case(&["--max-boots", "0"], "--max-boots"; "zero boots")]
#[test_case(&["--verify-every", "-1"], "-1"; "negative verify interval")]
#[test_case(&["--seed", "x"], "--seed"; "non-numeric seed")]
fn invalid_arguments_are_usage_errors_that_touch_nothing(args: &[&str], named: &str) {
    let mut host = Host::default();
    let ran = host.run(args.iter().copied().chain(["--state-dir", "/state"]));
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(ran.stderr.contains(named), "{}", ran.stderr);
    assert!(ran.stdout.is_empty());
    assert!(host.states.is_empty());
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

#[test_case(&["--machine", "/machine.json"]; "invalid machine")]
#[test_case(&["--replay-facts"]; "replay without a machine")]
fn invalid_input_creates_no_temporary_state(args: &[&str]) {
    let mut host = Host::default();
    host.put("/machine.json", r#"{"unknown_machine_key":1}"#);
    let ran = host.run(args);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(!ran.stderr.contains("state directory"));
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
