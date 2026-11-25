use std::path::Path;

use clap::{Parser as _, error::ErrorKind};
use test_case::test_case;
use togi_tool_sim::scripted::ScriptedCase;

use super::common::{Host, sim};
use crate::{Cli, Command};

#[test]
fn scripted_parses_the_child_protocol_into_typed_arguments() {
    let cli = Cli::try_parse_from([
        "togi-tool-sim",
        "scripted",
        "--case",
        "mce-resume",
        "--contract",
        "1",
        "--state-dir",
        "/case",
        "--max-boots",
        "7",
    ])
    .unwrap();
    let Some(Command::Scripted(args)) = cli.command else {
        panic!("expected the scripted command");
    };
    assert_eq!(args.case, ScriptedCase::MceResume);
    assert_eq!(args.contract, "1");
    assert_eq!(args.state_dir, Path::new("/case"));
    assert_eq!(args.max_boots.get(), 7);
}

#[test_case("unknown-case", "7", ErrorKind::InvalidValue; "unknown case")]
#[test_case("mce-resume", "0", ErrorKind::ValueValidation; "zero boot cap")]
fn scripted_refuses_invalid_typed_arguments(case: &str, boots: &str, expected: ErrorKind) {
    let error = Cli::try_parse_from([
        "togi-tool-sim",
        "scripted",
        "--case",
        case,
        "--contract",
        "1",
        "--state-dir",
        "/case",
        "--max-boots",
        boots,
    ])
    .unwrap_err();
    assert_eq!(error.kind(), expected);
}

#[test_case(&["--unknown"], "--unknown"; "unknown flag")]
#[test_case(&["extra"], "extra"; "positional argument")]
#[test_case(&["--night-hours", "0"], "--night-hours"; "zero night hours")]
#[test_case(&["--night-hours", "-1"], "-1"; "negative night hours")]
#[test_case(&["--max-boots", "0"], "--max-boots"; "zero boots")]
#[test_case(&["--verify-every", "-1"], "-1"; "negative verify interval")]
#[test_case(&["--seed", "x"], "--seed"; "non-numeric seed")]
#[test_case(&["--default-fitted-machine"], "--default-fitted-machine"; "retired fitted default")]
#[test_case(&["--machine"], "--machine"; "retired machine file")]
#[test_case(&["--replay-facts"], "--replay-facts"; "retired facts replay")]
#[test_case(&["suite"], "suite"; "retired suite")]
#[test_case(&["check"], "check"; "retired suite check")]
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
