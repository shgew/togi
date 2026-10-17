use std::path::{Path, PathBuf};

use test_case::test_case;
use togi_tool_sim::{Split, Suite, SuiteError};

fn parse(text: &str) -> Result<Suite, SuiteError> {
    Suite::parse(text, Path::new("bench"))
}

#[test_case("{" => matches SuiteError::Decode(_); "syntax")]
#[test_case(r#"{"scenarios":[],"gates":1}"# => matches SuiteError::Decode(_); "unknown suite key")]
#[test_case(r#"{"scenarios":[{"name":"x","dev":[1],"bogus":1}]}"# => matches SuiteError::Decode(_); "unknown scenario key")]
#[test_case(r#"{"scenarios":[{"name":"","dev":[1]}]}"# => matches SuiteError::InvalidName(_); "empty name")]
#[test_case(r#"{"scenarios":[{"name":"a/b","dev":[1]}]}"# => matches SuiteError::InvalidName(_); "nested name")]
#[test_case(r#"{"scenarios":[{"name":".","dev":[1]}]}"# => matches SuiteError::InvalidName(_); "dot name")]
#[test_case(r#"{"scenarios":[{"name":"..","dev":[1]}]}"# => matches SuiteError::InvalidName(_); "parent name")]
#[test_case(r#"{"scenarios":[{"name":"a\\b","dev":[1]}]}"# => matches SuiteError::InvalidName(_); "backslash name")]
#[test_case(r#"{"scenarios":[{"name":"x","dev":[1]},{"name":"x","dev":[2]}]}"# => matches SuiteError::DuplicateName(_); "duplicate name")]
#[test_case(r#"{"scenarios":[{"name":"x","machine":"a.json","machines":["b.json"],"dev":[1]}]}"# => matches SuiteError::MachineAndMachines(_); "machine and machines")]
#[test_case(r#"{"scenarios":[{"name":"x","machines":["a.json",""],"dev":[1]}]}"# => matches SuiteError::EmptyMachine(_); "empty member")]
#[test_case(r#"{"scenarios":[{"name":"x","dev":[1,1]}]}"# => matches SuiteError::RepeatedSeed { seed: 1, .. }; "repeated dev seed")]
#[test_case(r#"{"scenarios":[{"name":"x","holdout":[4,4]}]}"# => matches SuiteError::RepeatedSeed { seed: 4, .. }; "repeated holdout seed")]
#[test_case(r#"{"scenarios":[{"name":"x","dev":[1],"holdout":[1]}]}"# => matches SuiteError::RepeatedSeed { seed: 1, .. }; "overlapping splits")]
#[test_case(r#"{"scenarios":[{"name":"x","dev":[1],"smoke":[2]}]}"# => matches SuiteError::SmokeOutsideSplits { seed: 2, .. }; "smoke outside the splits")]
#[test_case(r#"{"scenarios":[{"name":"x"}]}"# => matches SuiteError::NoRuns; "no runs")]
#[test_case(r#"{"scenarios":[]}"# => matches SuiteError::NoRuns; "no scenarios")]
fn invalid_suites_are_refused(text: &str) -> SuiteError {
    parse(text).unwrap_err()
}

#[test]
fn a_missing_suite_file_is_a_read_error() {
    let error = Suite::load_with(Path::new("/absent.json"), |_| {
        Err(std::io::ErrorKind::NotFound.into())
    })
    .unwrap_err();
    assert!(matches!(error, SuiteError::Read { .. }), "{error}");
}

#[test]
fn a_suite_file_resolves_machines_beside_itself() {
    let path = Path::new("/suite/suite.json");
    let runs = Suite::load_with(path, |read_path| {
        assert_eq!(read_path, path);
        Ok(r#"{"scenarios":[{"name":"x","machine":"m/a.json","dev":[1]}]}"#.to_owned())
    })
    .unwrap()
    .runs();
    assert_eq!(runs[0].machine, Some(PathBuf::from("/suite/m/a.json")));
}

#[test]
fn smoke_seeds_mark_their_runs() {
    let suite =
        parse(r#"{"scenarios":[{"name":"x","dev":[1,2],"holdout":[3,4],"smoke":[2,3]}]}"#).unwrap();
    let flags: Vec<bool> = suite.runs().iter().map(|run| run.smoke).collect();
    assert_eq!(flags, [false, true, true, false]);
    let ids: Vec<String> = suite
        .runs()
        .iter()
        .filter(|run| run.smoke)
        .map(|run| run.id.to_string())
        .collect();
    assert_eq!(ids, ["x/dev-2", "x/holdout-3"]);
}

#[test]
fn runs_come_in_suite_order_with_dev_before_holdout() {
    let suite = parse(
        r#"{"scenarios":[
            {"name":"b","holdout":[9],"dev":[2,1]},
            {"name":"a","dev":[5]}
        ]}"#,
    )
    .unwrap();
    let ids: Vec<String> = suite.runs().iter().map(|run| run.id.to_string()).collect();
    assert_eq!(ids, ["b/dev-2", "b/dev-1", "b/holdout-9", "a/dev-5"]);
    assert_eq!(suite.runs()[2].split, Split::Holdout);
}

#[test]
fn ensemble_members_rotate_separately_within_each_split() {
    let suite = parse(
        r#"{"scenarios":[{"name":"x","machines":["a.json","b.json"],
            "dev":[1,2,3],"holdout":[10,11,12]}]}"#,
    )
    .unwrap();
    let members: Vec<PathBuf> = suite
        .runs()
        .into_iter()
        .map(|run| run.machine.unwrap())
        .collect();
    let expected: Vec<PathBuf> = ["a", "b", "a", "a", "b", "a"]
        .iter()
        .map(|name| Path::new("bench").join(format!("{name}.json")))
        .collect();
    assert_eq!(members, expected);
}

#[test]
fn an_empty_machine_means_the_default_machine() {
    let suite = parse(r#"{"scenarios":[{"name":"x","machine":"","dev":[1]}]}"#).unwrap();
    assert_eq!(suite.runs()[0].machine, None);
}

#[test]
fn replay_is_carried_to_every_run_of_its_scenario() {
    let suite =
        parse(r#"{"scenarios":[{"name":"x","replay":true,"dev":[1],"holdout":[2]},{"name":"y","dev":[1]}]}"#)
            .unwrap();
    let replay: Vec<bool> = suite.runs().iter().map(|run| run.replay).collect();
    assert_eq!(replay, [true, true, false]);
}

#[test]
fn a_gate_object_is_accepted_and_ignored() {
    let gated =
        parse(r#"{"gate":{"id":"g","split":"dev"},"scenarios":[{"name":"x","dev":[1]}]}"#).unwrap();
    assert_eq!(gated.runs().len(), 1);
}

#[test_case(r#"{"gate":1,"scenarios":[{"name":"x","dev":[1]}]}"#; "number")]
#[test_case(r#"{"gate":["g"],"scenarios":[{"name":"x","dev":[1]}]}"#; "array")]
fn a_gate_that_is_not_an_object_is_refused(text: &str) {
    assert!(matches!(parse(text), Err(SuiteError::Decode(_))));
}

#[test]
fn a_suite_and_its_scenarios_are_objects() {
    let object = r#"{"scenarios":[{"name":"x","machine":"","machines":[],"replay":false,"dev":[1],"holdout":[],"smoke":[]}]}"#;
    assert_eq!(parse(object).unwrap().runs().len(), 1);
}

#[test_case(r#"[[{"name":"x","dev":[1]}]]"#; "suite")]
#[test_case(r#"{"scenarios":[["x","",[],false,[1],[],[]]]}"#; "scenario")]
#[test_case(r#"[[["x","",[],false,[1],[],[]]]]"#; "both")]
fn a_suite_or_scenario_is_never_a_positional_array(text: &str) {
    assert!(matches!(parse(text), Err(SuiteError::Decode(_))));
}
