use std::path::{Path, PathBuf};

use togi_tool_sim::{Split, Suite, SuiteError};

fn parse(text: &str) -> Result<Suite, SuiteError> {
    Suite::parse(text, Path::new("bench"))
}

#[test]
fn invalid_suites_are_refused() {
    type Case = (&'static str, String, fn(&SuiteError) -> bool);
    let cases: Vec<Case> = vec![
        ("syntax", "{".into(), |e| matches!(e, SuiteError::Decode(_))),
        (
            "unknown suite key",
            r#"{"scenarios":[],"gates":1}"#.into(),
            |e| matches!(e, SuiteError::Decode(_)),
        ),
        (
            "unknown scenario key",
            r#"{"scenarios":[{"name":"x","dev":[1],"bogus":1}]}"#.into(),
            |e| matches!(e, SuiteError::Decode(_)),
        ),
        (
            "empty name",
            r#"{"scenarios":[{"name":"","dev":[1]}]}"#.into(),
            |e| matches!(e, SuiteError::InvalidName(_)),
        ),
        (
            "nested name",
            r#"{"scenarios":[{"name":"a/b","dev":[1]}]}"#.into(),
            |e| matches!(e, SuiteError::InvalidName(_)),
        ),
        (
            "dot name",
            r#"{"scenarios":[{"name":".","dev":[1]}]}"#.into(),
            |e| matches!(e, SuiteError::InvalidName(_)),
        ),
        (
            "parent name",
            r#"{"scenarios":[{"name":"..","dev":[1]}]}"#.into(),
            |e| matches!(e, SuiteError::InvalidName(_)),
        ),
        (
            "backslash name",
            r#"{"scenarios":[{"name":"a\\b","dev":[1]}]}"#.into(),
            |e| matches!(e, SuiteError::InvalidName(_)),
        ),
        (
            "duplicate name",
            r#"{"scenarios":[{"name":"x","dev":[1]},{"name":"x","dev":[2]}]}"#.into(),
            |e| matches!(e, SuiteError::DuplicateName(_)),
        ),
        (
            "machine and machines",
            r#"{"scenarios":[{"name":"x","machine":"a.json","machines":["b.json"],"dev":[1]}]}"#
                .into(),
            |e| matches!(e, SuiteError::MachineAndMachines(_)),
        ),
        (
            "empty member",
            r#"{"scenarios":[{"name":"x","machines":["a.json",""],"dev":[1]}]}"#.into(),
            |e| matches!(e, SuiteError::EmptyMachine(_)),
        ),
        (
            "repeated dev seed",
            r#"{"scenarios":[{"name":"x","dev":[1,1]}]}"#.into(),
            |e| matches!(e, SuiteError::RepeatedSeed { seed: 1, .. }),
        ),
        (
            "repeated holdout seed",
            r#"{"scenarios":[{"name":"x","holdout":[4,4]}]}"#.into(),
            |e| matches!(e, SuiteError::RepeatedSeed { seed: 4, .. }),
        ),
        (
            "overlapping splits",
            r#"{"scenarios":[{"name":"x","dev":[1],"holdout":[1]}]}"#.into(),
            |e| matches!(e, SuiteError::RepeatedSeed { seed: 1, .. }),
        ),
        (
            "smoke outside the splits",
            r#"{"scenarios":[{"name":"x","dev":[1],"smoke":[2]}]}"#.into(),
            |e| matches!(e, SuiteError::SmokeOutsideSplits { seed: 2, .. }),
        ),
        ("no runs", r#"{"scenarios":[{"name":"x"}]}"#.into(), |e| {
            matches!(e, SuiteError::NoRuns)
        }),
        ("no scenarios", r#"{"scenarios":[]}"#.into(), |e| {
            matches!(e, SuiteError::NoRuns)
        }),
    ];
    for (name, text, expected) in cases {
        let error = parse(&text).unwrap_err();
        assert!(expected(&error), "{name}: {error}");
    }
}

#[test]
fn a_missing_suite_file_is_a_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = Suite::load(&dir.path().join("absent.json")).unwrap_err();
    assert!(matches!(error, SuiteError::Read { .. }), "{error}");
}

#[test]
fn a_suite_file_resolves_machines_beside_itself() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("suite.json");
    std::fs::write(
        &path,
        r#"{"scenarios":[{"name":"x","machine":"m/a.json","dev":[1]}]}"#,
    )
    .unwrap();
    let runs = Suite::load(&path).unwrap().runs();
    assert_eq!(runs[0].machine, Some(dir.path().join("m/a.json")));
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
fn a_gate_object_is_accepted_and_ignored_but_never_anything_else() {
    let gated =
        parse(r#"{"gate":{"id":"g","split":"dev"},"scenarios":[{"name":"x","dev":[1]}]}"#).unwrap();
    assert_eq!(gated.runs().len(), 1);
    for text in [
        r#"{"gate":1,"scenarios":[{"name":"x","dev":[1]}]}"#,
        r#"{"gate":["g"],"scenarios":[{"name":"x","dev":[1]}]}"#,
    ] {
        assert!(matches!(parse(text), Err(SuiteError::Decode(_))), "{text}");
    }
}

#[test]
fn a_suite_and_its_scenarios_are_objects_never_positional_arrays() {
    let object = r#"{"scenarios":[{"name":"x","machine":"","machines":[],"replay":false,"dev":[1],"holdout":[],"smoke":[]}]}"#;
    assert_eq!(parse(object).unwrap().runs().len(), 1);
    for (case, text) in [
        ("suite", r#"[[{"name":"x","dev":[1]}]]"#),
        ("scenario", r#"{"scenarios":[["x","",[],false,[1],[],[]]]}"#),
        ("both", r#"[[["x","",[],false,[1],[],[]]]]"#),
    ] {
        assert!(
            matches!(parse(text), Err(SuiteError::Decode(_))),
            "{case}: {text}"
        );
    }
}
