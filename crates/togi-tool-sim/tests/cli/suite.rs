use std::fs;
use std::path::Path;

use togi_tool_sim::{Outcome, parse_manifest};

use crate::common::{entries, sim};

const SUITE: &str =
    r#"{"scenarios":[{"name":"default","machine":"","dev":[1,2],"holdout":[101]}]}"#;

fn suite(dir: &Path) -> String {
    let path = dir.join("suite.json");
    fs::write(&path, SUITE).unwrap();
    path.to_str().unwrap().to_owned()
}

fn run_shard(suite: &str, out: &Path, shard: &str, extra: &[&str]) -> crate::common::Ran {
    let mut args = vec![
        "suite",
        "--suite",
        suite,
        "--shards",
        "2",
        "--shard",
        shard,
        "--out",
        out.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    sim(&args)
}

fn check(suite: &str, manifests: &Path) -> crate::common::Ran {
    sim([
        "check",
        "--suite",
        suite,
        "--shards",
        "2",
        manifests.to_str().unwrap(),
    ])
}

#[test]
fn every_shard_runs_its_sessions_clean_and_the_check_proves_coverage() {
    let root = tempfile::tempdir().unwrap();
    let suite = suite(root.path());
    let out = root.path().join("manifests");
    for shard in ["0", "1"] {
        let ran = run_shard(&suite, &out, shard, &[]);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    }
    let names: Vec<_> = entries(&out)
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["shard-0.jsonl", "shard-1.jsonl"]);
    let mut ids: Vec<String> = Vec::new();
    for path in entries(&out) {
        for result in parse_manifest(&fs::read_to_string(path).unwrap()).unwrap() {
            assert_eq!(result.outcome, Outcome::Clean, "{}", result.id);
            ids.push(result.id.to_string());
        }
    }
    ids.sort();
    assert_eq!(
        ids,
        ["default/dev-1", "default/dev-2", "default/holdout-101"]
    );
    let ran = check(&suite, &out);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);

    let swap = out.join("swap");
    fs::rename(out.join("shard-0.jsonl"), &swap).unwrap();
    fs::rename(out.join("shard-1.jsonl"), out.join("shard-0.jsonl")).unwrap();
    fs::rename(&swap, out.join("shard-1.jsonl")).unwrap();
    let ran = check(&suite, &out);
    assert_eq!(ran.code, Some(1));
    assert!(
        ran.stderr.contains("which the partition gives shard"),
        "{}",
        ran.stderr
    );
    fs::rename(out.join("shard-0.jsonl"), &swap).unwrap();
    fs::rename(out.join("shard-1.jsonl"), out.join("shard-0.jsonl")).unwrap();
    fs::rename(&swap, out.join("shard-1.jsonl")).unwrap();

    fs::remove_file(out.join("shard-1.jsonl")).unwrap();
    let ran = check(&suite, &out);
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
    let root = tempfile::tempdir().unwrap();
    let suite = suite(root.path());
    let out = root.path().join("manifests");
    fs::create_dir(&out).unwrap();
    fs::write(out.join("shard-0.jsonl"), "").unwrap();
    fs::write(out.join("shard-01.jsonl"), "").unwrap();
    let ran = check(&suite, &out);
    assert_eq!(ran.code, Some(1));
    assert!(ran.stderr.contains("shard-01.jsonl"), "{}", ran.stderr);
}

#[test]
fn a_session_that_does_not_end_clean_fails_its_shard_and_is_recorded() {
    let root = tempfile::tempdir().unwrap();
    let suite = suite(root.path());
    let out = root.path().join("manifests");
    let ran = run_shard(&suite, &out, "0", &["--max-boots", "1"]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    let results = parse_manifest(&fs::read_to_string(out.join("shard-0.jsonl")).unwrap()).unwrap();
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
    let root = tempfile::tempdir().unwrap();
    let suite = suite(root.path());
    let out = root.path().join("manifests");
    let ran = run_shard(&suite, &out, "2", &[]);
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(!out.exists());
}

#[test]
fn the_manifest_does_not_depend_on_the_number_of_jobs() {
    let root = tempfile::tempdir().unwrap();
    let suite = suite(root.path());
    let mut manifests = Vec::new();
    for jobs in ["1", "3"] {
        let out = root.path().join(format!("jobs-{jobs}"));
        let ran = sim([
            "suite",
            "--suite",
            &suite,
            "--out",
            out.to_str().unwrap(),
            "--jobs",
            jobs,
        ]);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(ran.stderr.lines().count(), 3, "{}", ran.stderr);
        manifests.push(fs::read(out.join("shard-0.jsonl")).unwrap());
    }
    assert_eq!(manifests[0], manifests[1]);
}

#[test]
fn a_shard_is_checked_alone_against_exactly_its_own_runs() {
    let root = tempfile::tempdir().unwrap();
    let suite = suite(root.path());
    let out = root.path().join("manifests");
    let ran = run_shard(&suite, &out, "1", &[]);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let alone = |shard: &str| {
        sim([
            "check",
            "--suite",
            &suite,
            "--shards",
            "2",
            "--shard",
            shard,
            out.to_str().unwrap(),
        ])
    };
    assert_eq!(alone("1").code, Some(0));
    assert_eq!(check(&suite, &out).code, Some(1));
    let ran = alone("0");
    assert_eq!(ran.code, Some(1));
    assert!(
        ran.stderr.contains("shard 0 has no manifest"),
        "{}",
        ran.stderr
    );
    let path = out.join("shard-1.jsonl");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("{\"id\":\"default/dev-1\",\"outcome\":{\"kind\":\"clean\"}}\n");
    fs::write(&path, text).unwrap();
    let ran = alone("1");
    assert_eq!(ran.code, Some(1));
    assert!(
        ran.stderr
            .contains("reports run default/dev-1, which is not one of the runs checked"),
        "{}",
        ran.stderr
    );
}
