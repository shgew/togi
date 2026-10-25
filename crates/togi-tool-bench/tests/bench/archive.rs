use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use crate::common;
use serde_json::Value;
use sha2::{Digest, Sha256};
use test_case::test_case;
use togi_tool_bench::{Selection, read_records_from};

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").unwrap();
            hex
        })
}

#[test]
fn the_archived_ruleset_11_gate_is_kept_unchanged() {
    for (name, bytes, want) in [
        (
            "suite.json",
            include_bytes!("../../../../tools/bench/gates/ruleset-11/suite.json").as_slice(),
            "afcd7feacbd6a5ddbaf30f1d69b2750749d92f30d52bde95816168ef2f2c895b",
        ),
        (
            "baseline.jsonl",
            include_bytes!("../../../../tools/bench/gates/ruleset-11/baseline.jsonl").as_slice(),
            "4cb9921ec9f31bba9899e3c4cd6dc0ebe4cbead1039fd5947f3f39ec8fb1f78c",
        ),
        (
            "candidate.jsonl",
            include_bytes!("../../../../tools/bench/gates/ruleset-11/candidate.jsonl").as_slice(),
            "6aaa3781211f46b207af2ef8b6810c7cd398b9641ea227ddfedb1bdf7352fc05",
        ),
        (
            "report.txt",
            include_bytes!("../../../../tools/bench/gates/ruleset-11/report.txt").as_slice(),
            "68a1af81c8de041ca863ac004dbac4de83c2d854ada2588558eed9040516e7c6",
        ),
    ] {
        assert_eq!(
            sha256(bytes),
            want,
            "{name}: the archive records what was scored and never changes"
        );
    }
}

#[test_case("baseline.jsonl", include_bytes!("../../../../tools/bench/gates/ruleset-11/baseline.jsonl").as_slice(); "baseline")]
#[test_case("candidate.jsonl", include_bytes!("../../../../tools/bench/gates/ruleset-11/candidate.jsonl").as_slice(); "candidate")]
fn archived_go_records_are_not_accepted(name: &str, bytes: &[u8]) {
    let error = read_records_from(bytes, Path::new(name), "records")
        .unwrap_err()
        .to_string();
    assert!(error.contains("line 1"), "{name}: {error}");
}

#[test]
fn the_committed_baseline_covers_every_run_of_the_suite() {
    let tree = common::Tree::committed();
    let plan = tree.suite().plan_from(Selection::All, &tree).unwrap();
    let text = include_str!("../../../../tools/bench/baseline.jsonl");
    let have: BTreeSet<(String, u64)> = text
        .lines()
        .map(|line| {
            let run: Value = serde_json::from_str(line).unwrap();
            (
                run["scenario"].as_str().unwrap().to_owned(),
                run["seed"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(have.len(), text.lines().count(), "no run twice");
    assert_eq!(have.len(), plan.runs.len());
    for run in &plan.runs {
        assert!(
            have.contains(&(run.scenario.clone(), run.seed)),
            "{}",
            run.name()
        );
    }
}
