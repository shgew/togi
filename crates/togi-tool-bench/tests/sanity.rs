#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

mod common;

use std::fs;
use std::path::Path;

use common::record;
use togi_tool_bench::sanity::{GoRun, compare, read_go, report};
use togi_tool_bench::{Record, Status};

fn go_runs(lines: &[String]) -> Vec<GoRun> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("baseline.jsonl");
    fs::write(&path, lines.join("\n")).unwrap();
    read_go(&path).unwrap()
}

fn go_line(scenario: &str, seed: u64, sim_hours: f64, crashes: u32) -> String {
    format!(
        r#"{{"scenario":"{scenario}","machine":"","seed":{seed},"split":"dev","commit":"5145f657","dirty":false,"ruleset":11,"status":"concluded","exit_code":0,"sim_hours":{sim_hours},"first_passed_cycle_h":null,"passed_cycles":2,"crashes":{crashes},"trials":1000,"real_answers":0,"real_answer_share":0,"scenario_real_answer_share":0,"trial_hours":40.0,"hunts":2,"combinations":0,"final_profile":[-1,-2],"depth":-300,"hazard_per_h":{{"R1":0}},"hazard_max_per_h":0,"partial_seconds":150}}"#
    )
}

fn rust(scenario: &str, seed: u64, sim_hours: f64, crashes: u32) -> Record {
    Record {
        sim_hours,
        crashes,
        trials: 1000,
        passed_cycles: 2,
        trial_hours: 40.0,
        hunts: 2,
        depth: -300,
        ..record(scenario, seed)
    }
}

fn series(scenario: &str, hours: &[f64]) -> (Vec<Record>, Vec<String>) {
    let rust_runs = hours
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust(scenario, seed, h, 10))
        .collect();
    let go_lines = hours
        .iter()
        .zip(1..)
        .map(|(&h, seed)| go_line(scenario, seed, h, 10))
        .collect();
    (rust_runs, go_lines)
}

fn flagged(lines: &[togi_tool_bench::sanity::Line]) -> Vec<(String, &'static str)> {
    lines
        .iter()
        .filter(|l| l.flagged)
        .map(|l| (l.scenario.clone(), l.metric))
        .collect()
}

#[test]
fn go_scores_ignore_the_fields_the_rust_bench_does_not_compare() {
    let runs = go_runs(&[go_line("default", 1, 48.0, 9)]);
    assert_eq!(runs.len(), 1);
}

#[test]
fn a_malformed_go_line_names_its_number_and_a_missing_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("baseline.jsonl");
    fs::write(&path, format!("{}\n{{", go_line("default", 1, 48.0, 9))).unwrap();
    let error = read_go(&path).unwrap_err().to_string();
    assert!(error.contains("line 2"), "{error}");
    assert!(read_go(&dir.path().join("missing.jsonl")).is_err());
}

#[test]
fn identical_distributions_flag_nothing() {
    let (rust_runs, go_lines) = series("default", &[40.0, 45.0, 50.0, 55.0, 60.0]);
    let lines = compare(&rust_runs, &go_runs(&go_lines));
    assert!(!lines.is_empty());
    assert!(flagged(&lines).is_empty());
    assert!(
        lines
            .iter()
            .all(|l| l.z.abs() < f64::EPSILON || l.metric == "first_passed_cycle_h")
    );
}

#[test]
fn a_shift_beyond_the_band_and_the_spread_is_flagged() {
    let (_, go_lines) = series("default", &[40.0, 41.0, 42.0, 43.0, 44.0, 45.0]);
    let shifted: Vec<Record> = [60.0, 61.0, 62.0, 63.0, 64.0, 65.0]
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust("default", seed, h, 10))
        .collect();
    let lines = compare(&shifted, &go_runs(&go_lines));
    assert_eq!(
        flagged(&lines),
        [
            ("default".to_owned(), "sim_hours"),
            ("TOTAL".to_owned(), "sim_hours")
        ]
    );
}

#[test]
fn a_shift_inside_the_band_is_not_flagged_however_tight_the_spread() {
    let (_, go_lines) = series("default", &[40.0, 41.0, 42.0, 43.0, 44.0, 45.0]);
    let shifted: Vec<Record> = [42.0, 43.0, 44.0, 45.0, 46.0, 47.0]
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust("default", seed, h, 10))
        .collect();
    assert!(flagged(&compare(&shifted, &go_runs(&go_lines))).is_empty());
}

#[test]
fn a_shift_the_spread_explains_is_not_flagged_however_large() {
    let (_, go_lines) = series("default", &[10.0, 90.0, 20.0, 80.0]);
    let shifted: Vec<Record> = [30.0, 100.0, 40.0, 95.0]
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust("default", seed, h, 10))
        .collect();
    assert!(flagged(&compare(&shifted, &go_runs(&go_lines))).is_empty());
}

#[test]
fn one_run_has_no_spread_to_judge_a_difference_by() {
    let rust_runs = [rust("default", 1, 100.0, 10)];
    let lines = compare(&rust_runs, &go_runs(&[go_line("default", 1, 40.0, 10)]));
    assert!(flagged(&lines).is_empty());
}

#[test]
fn a_lost_conclusion_is_flagged() {
    let (mut rust_runs, go_lines) = series("default", &[40.0, 41.0, 42.0, 43.0, 44.0, 45.0]);
    for record in &mut rust_runs[..3] {
        record.status = Status::Censored;
    }
    let lines = compare(&rust_runs, &go_runs(&go_lines));
    assert!(flagged(&lines).contains(&("default".to_owned(), "concluded")));
}

#[test]
fn only_runs_both_benches_ran_are_compared_in_the_order_the_records_name_scenarios() {
    let (zeta_rust, zeta_go) = series("zeta", &[10.0, 11.0]);
    let (alpha_rust, alpha_go) = series("alpha", &[20.0, 21.0]);
    let mut rust_runs = zeta_rust;
    rust_runs.extend(alpha_rust);
    rust_runs.push(rust("only-rust", 1, 5.0, 1));
    let mut go_lines = alpha_go;
    go_lines.extend(zeta_go);
    go_lines.push(go_line("only-go", 1, 5.0, 1));
    go_lines.push(go_line("alpha", 99, 1000.0, 1));
    let lines = compare(&rust_runs, &go_runs(&go_lines));
    let mut scenarios: Vec<&str> = Vec::new();
    for line in &lines {
        if !scenarios.contains(&line.scenario.as_str()) {
            scenarios.push(&line.scenario);
        }
    }
    assert_eq!(scenarios, ["zeta", "alpha", "TOTAL"]);
    assert!(
        lines
            .iter()
            .filter(|l| l.metric == "sim_hours")
            .all(|l| l.rust_runs == l.go_runs)
    );
    assert!(flagged(&lines).is_empty());
}

#[test]
fn nothing_shared_compares_nothing() {
    let rust_runs = [rust("a", 1, 1.0, 1)];
    assert!(compare(&rust_runs, &go_runs(&[go_line("b", 1, 1.0, 1)])).is_empty());
}

#[test]
fn a_metric_one_side_never_measured_is_left_out() {
    let (mut rust_runs, go_lines) = series("default", &[40.0, 41.0]);
    rust_runs[0].worst_r7_hazard_per_h = Some(1.0);
    let lines = compare(&rust_runs, &go_runs(&go_lines));
    assert!(lines.iter().all(|l| l.metric != "worst_r7_hazard_per_h"));
}

#[test]
fn the_table_marks_flagged_lines_and_counts_them() {
    let (_, go_lines) = series("default", &[40.0, 41.0, 42.0, 43.0]);
    let shifted: Vec<Record> = [60.0, 61.0, 62.0, 63.0]
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust("default", seed, h, 10))
        .collect();
    let lines = compare(&shifted, &go_runs(&go_lines));
    let mut out = Vec::new();
    report(&mut out, &lines).unwrap();
    insta::assert_snapshot!(String::from_utf8(out).unwrap());
}

#[test]
fn the_committed_go_baseline_is_the_whole_suite_without_deleted_scenarios() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/baseline.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let runs = read_go(&path).unwrap();
    assert_eq!(runs.len(), 220);
    assert_eq!(
        text.matches("\"scenario\":\"target-nonmember-mce\"")
            .count(),
        8
    );
    for deleted in [
        "target-r4-limit",
        "target-delayed-joint",
        "target-flat-risk",
        "target-flat-cost",
    ] {
        assert!(!text.contains(deleted), "{deleted}");
    }
}
