use std::path::Path;

use test_case::test_case;
use togi_journal::DeadEndCondition;
use togi_tool_sim::{Outcome, parse_manifest};

use super::common::{Host, Ran};

const SUITE: &str =
    r#"{"scenarios":[{"name":"default","machine":"","dev":[1,2],"holdout":[101]}]}"#;

fn host() -> Host {
    let mut host = Host::default();
    host.put("/suite.json", SUITE);
    host
}

fn run_shard(host: &mut Host, shard: &str, extra: &[&str]) -> Ran {
    let mut args = vec![
        "suite",
        "--suite",
        "/suite.json",
        "--shards",
        "2",
        "--shard",
        shard,
        "--out",
        "/manifests",
    ];
    args.extend_from_slice(extra);
    host.run(args)
}

fn check(host: &mut Host) -> Ran {
    host.run([
        "check",
        "--suite",
        "/suite.json",
        "--shards",
        "2",
        "/manifests",
    ])
}

fn swap(host: &mut Host) {
    let first = host
        .files
        .remove(Path::new("/manifests/shard-0.jsonl"))
        .unwrap();
    let second = host
        .files
        .remove(Path::new("/manifests/shard-1.jsonl"))
        .unwrap();
    host.put("/manifests/shard-0.jsonl", second);
    host.put("/manifests/shard-1.jsonl", first);
}

#[test]
fn every_shard_runs_its_sessions_clean_and_the_check_proves_coverage() {
    let mut host = host();
    for shard in ["0", "1"] {
        let ran = run_shard(&mut host, shard, &[]);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    }
    let names: Vec<_> = host
        .files
        .keys()
        .filter(|path| path.parent() == Some(Path::new("/manifests")))
        .map(|path| path.file_name().unwrap().to_str().unwrap())
        .collect();
    assert_eq!(names, ["shard-0.jsonl", "shard-1.jsonl"]);
    let mut ids = Vec::new();
    for (path, text) in &host.files {
        if path.parent() != Some(Path::new("/manifests")) {
            continue;
        }
        for result in parse_manifest(text).unwrap() {
            assert_eq!(result.outcome, Outcome::Clean, "{}", result.id);
            ids.push(result.id.to_string());
        }
    }
    ids.sort();
    assert_eq!(
        ids,
        ["default/dev-1", "default/dev-2", "default/holdout-101"]
    );
    assert_eq!(check(&mut host).code, Some(0));
    swap(&mut host);
    let ran = check(&mut host);
    assert_eq!(ran.code, Some(1));
    assert!(
        ran.stderr.contains("which the partition gives shard"),
        "{}",
        ran.stderr
    );
    swap(&mut host);
    host.files.remove(Path::new("/manifests/shard-1.jsonl"));
    let ran = check(&mut host);
    assert_eq!(ran.code, Some(1));
    assert!(
        ran.stderr.contains("shard 1 has no manifest"),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.contains("is in no manifest"), "{}", ran.stderr);
}

#[test]
fn a_stray_file_among_the_manifests_fails_the_check() {
    let mut host = host();
    host.put("/manifests/shard-0.jsonl", "");
    host.put("/manifests/shard-01.jsonl", "");
    let ran = check(&mut host);
    assert_eq!(ran.code, Some(1));
    assert!(ran.stderr.contains("shard-01.jsonl"), "{}", ran.stderr);
}

#[test]
fn a_malformed_manifest_line_fails_the_check_naming_the_line_and_its_cause() {
    let mut host = host();
    host.put("/manifests/shard-0.jsonl", "\n{");
    let ran = check(&mut host);
    assert_eq!(ran.code, Some(1));
    assert_eq!(
        ran.stderr,
        "sim: /manifests/shard-0.jsonl: manifest line 2: EOF while parsing an object at line 1 column 1\n"
    );
}

#[test]
fn a_session_that_does_not_end_clean_fails_its_shard_and_is_recorded() {
    let mut host = host();
    let ran = run_shard(&mut host, "0", &["--max-boots", "1"]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    let results = parse_manifest(&host.files[Path::new("/manifests/shard-0.jsonl")]).unwrap();
    assert!(
        results
            .iter()
            .any(|result| result.outcome == Outcome::Capped { boots: 1 }),
        "{results:?}"
    );
    assert!(
        ran.stderr.contains("reached the cap of 1 boots"),
        "{}",
        ran.stderr
    );
}

#[test]
fn a_shard_outside_the_count_is_refused() {
    let mut host = host();
    let ran = run_shard(&mut host, "2", &[]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(!host.files.keys().any(|path| path.starts_with("/manifests")));
}

#[test_case("2"; "equal to the count")]
#[test_case("3"; "past the count")]
#[test_case(&usize::MAX.to_string(); "largest index")]
fn a_shard_the_count_cannot_hold_is_refused_by_check(shard: &str) {
    let ran = host().run([
        "check",
        "--suite",
        "/suite.json",
        "--shards",
        "2",
        "--shard",
        shard,
        "/manifests",
    ]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
}

#[test]
fn a_session_that_dead_ends_fails_its_shard_and_is_recorded() {
    let mut host = host();
    let mut machine: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../tools/bench/machines/flat-hazard.json"
    ))
    .unwrap();
    machine["core"][0]["alone"] = serde_json::json!([1, 1, 1, 1, 1]);
    host.put("/machine.json", machine.to_string());
    host.put(
        "/suite.json",
        r#"{"scenarios":[{"name":"doomed","machine":"machine.json","dev":[1]}]}"#,
    );
    let ran = run_shard(&mut host, "0", &[]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    let results = parse_manifest(&host.files[Path::new("/manifests/shard-0.jsonl")]).unwrap();
    assert!(
        results.iter().any(|result| matches!(
            result.outcome,
            Outcome::DeadEnd {
                condition: DeadEndCondition::FailureAtZero,
                ..
            }
        )),
        "{results:?}"
    );
}

#[test]
fn the_manifest_does_not_depend_on_the_number_of_jobs() {
    let mut host = host();
    let mut manifests = Vec::new();
    for jobs in ["1", "3"] {
        let ran = host.run([
            "suite",
            "--suite",
            "/suite.json",
            "--out",
            "/manifests",
            "--jobs",
            jobs,
        ]);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(ran.stderr.lines().count(), 3, "{}", ran.stderr);
        manifests.push(host.files[Path::new("/manifests/shard-0.jsonl")].clone());
    }
    assert_eq!(manifests[0], manifests[1]);
}

#[test]
fn a_shard_is_checked_alone_against_exactly_its_own_runs() {
    let mut host = host();
    let ran = run_shard(&mut host, "1", &[]);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let alone = |host: &mut Host, shard: &str| {
        host.run([
            "check",
            "--suite",
            "/suite.json",
            "--shards",
            "2",
            "--shard",
            shard,
            "/manifests",
        ])
    };
    assert_eq!(alone(&mut host, "1").code, Some(0));
    assert_eq!(check(&mut host).code, Some(1));
    let ran = alone(&mut host, "0");
    assert_eq!(ran.code, Some(1));
    assert!(
        ran.stderr.contains("shard 0 has no manifest"),
        "{}",
        ran.stderr
    );
    host.files
        .get_mut(Path::new("/manifests/shard-1.jsonl"))
        .unwrap()
        .push_str("{\"id\":\"default/dev-1\",\"outcome\":{\"kind\":\"clean\"}}\n");
    let ran = alone(&mut host, "1");
    assert_eq!(ran.code, Some(1));
    assert!(
        ran.stderr
            .contains("reports run default/dev-1, which is not one of the runs checked"),
        "{}",
        ran.stderr
    );
}
