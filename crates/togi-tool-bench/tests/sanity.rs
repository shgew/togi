#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

mod common;

use std::fs;
use std::path::Path;

use common::record;
use togi_tool_bench::sanity::{BaselineRun, compare, read_baseline, report};
use togi_tool_bench::{Record, Status};

fn baseline_runs(lines: &[String]) -> Vec<BaselineRun> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("baseline.jsonl");
    fs::write(&path, lines.join("\n")).unwrap();
    read_baseline(&path).unwrap()
}

fn baseline_line(scenario: &str, seed: u64, sim_hours: f64, crashes: u32) -> String {
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
    let baseline_lines = hours
        .iter()
        .zip(1..)
        .map(|(&h, seed)| baseline_line(scenario, seed, h, 10))
        .collect();
    (rust_runs, baseline_lines)
}

fn flagged(lines: &[togi_tool_bench::sanity::Line]) -> Vec<(String, &'static str)> {
    lines
        .iter()
        .filter(|l| l.flagged)
        .map(|l| (l.scenario.clone(), l.metric))
        .collect()
}

const DISTINCT: &str = r#"{"scenario":"default","seed":1,"status":"concluded","sim_hours":41.5,"first_passed_cycle_h":7.25,"passed_cycles":3,"crashes":11,"trials":1234,"real_answers":617,"trial_hours":33.5,"hunts":5,"combinations":7,"depth":-321,"hazard_max_per_h":0.0625,"worst_r7_hazard_per_h":1.75,"split":"dev","retired_field":9,"final_profile":[-1]}"#;

#[test]
fn baseline_scores_ignore_the_fields_the_rust_bench_does_not_compare() {
    let runs = baseline_runs(&[DISTINCT.to_owned()]);
    assert_eq!(runs.len(), 1);
    let counterpart = Record {
        first_passed_cycle_h: Some(1.0),
        worst_r7_hazard_per_h: Some(1.0),
        ..record("default", 1)
    };
    let lines = compare(&[counterpart], &runs);
    let baseline = |metric| find(&lines, "default", metric).baseline;
    for (metric, want) in [
        ("concluded", 1.0),
        ("sim_hours", 41.5),
        ("first_passed_cycle_h", 7.25),
        ("passed_cycles", 3.0),
        ("crashes", 11.0),
        ("trials", 1234.0),
        ("real_answer_share", 0.5),
        ("trial_hours", 33.5),
        ("hunts", 5.0),
        ("combinations", 7.0),
        ("depth", -321.0),
        ("hazard_max_per_h", 0.0625),
        ("worst_r7_hazard_per_h", 1.75),
    ] {
        assert!(
            (baseline(metric) - want).abs() < 1e-12,
            "{metric}: {}",
            baseline(metric)
        );
    }
}

#[test]
fn a_malformed_baseline_line_names_its_number_and_a_missing_file_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("baseline.jsonl");
    fs::write(
        &path,
        format!("{}\n{{", baseline_line("default", 1, 48.0, 9)),
    )
    .unwrap();
    let error = read_baseline(&path).unwrap_err().to_string();
    assert!(error.contains("line 2"), "{error}");
    assert!(read_baseline(&dir.path().join("missing.jsonl")).is_err());
}

#[test]
fn identical_distributions_flag_nothing() {
    let (rust_runs, baseline_lines) = series("default", &[40.0, 45.0, 50.0, 55.0, 60.0]);
    let lines = compare(&rust_runs, &baseline_runs(&baseline_lines));
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
    let (_, baseline_lines) = series("default", &[40.0, 41.0, 42.0, 43.0, 44.0, 45.0]);
    let shifted: Vec<Record> = [60.0, 61.0, 62.0, 63.0, 64.0, 65.0]
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust("default", seed, h, 10))
        .collect();
    let lines = compare(&shifted, &baseline_runs(&baseline_lines));
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
    let (_, baseline_lines) = series("default", &[40.0, 41.0, 42.0, 43.0, 44.0, 45.0]);
    let shifted: Vec<Record> = [42.0, 43.0, 44.0, 45.0, 46.0, 47.0]
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust("default", seed, h, 10))
        .collect();
    assert!(flagged(&compare(&shifted, &baseline_runs(&baseline_lines))).is_empty());
}

#[test]
fn a_shift_the_spread_explains_is_not_flagged_however_large() {
    let (_, baseline_lines) = series("default", &[10.0, 90.0, 20.0, 80.0]);
    let shifted: Vec<Record> = [30.0, 100.0, 40.0, 95.0]
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust("default", seed, h, 10))
        .collect();
    assert!(flagged(&compare(&shifted, &baseline_runs(&baseline_lines))).is_empty());
}

#[test]
fn one_run_has_no_spread_to_judge_a_difference_by() {
    let rust_runs = [rust("default", 1, 100.0, 10)];
    let lines = compare(
        &rust_runs,
        &baseline_runs(&[baseline_line("default", 1, 40.0, 10)]),
    );
    assert!(flagged(&lines).is_empty());
}

#[test]
fn a_lost_conclusion_is_flagged() {
    let (mut rust_runs, baseline_lines) = series("default", &[40.0, 41.0, 42.0, 43.0, 44.0, 45.0]);
    for record in &mut rust_runs[..3] {
        record.status = Status::Censored;
    }
    let lines = compare(&rust_runs, &baseline_runs(&baseline_lines));
    assert!(flagged(&lines).contains(&("default".to_owned(), "concluded")));
}

#[test]
fn one_lost_conclusion_in_a_24_run_group_is_flagged() {
    let hours: Vec<f64> = (0..24).map(|i| 40.0 + f64::from(i % 6)).collect();
    let (mut rust_runs, baseline_lines) = series("default", &hours);
    rust_runs[0].status = Status::DeadEnd;
    let lines = compare(&rust_runs, &baseline_runs(&baseline_lines));
    let l = find(&lines, "default", "concluded");
    assert!(l.flagged);
    assert!((l.baseline - 1.0).abs() < f64::EPSILON);
    assert_eq!(l.rust_runs, 24);
    assert!(flagged(&lines).contains(&("TOTAL".to_owned(), "concluded")));
}

#[test]
fn more_conclusions_than_the_baseline_is_not_flagged() {
    let (mut rust_runs, baseline_lines) = series("default", &[40.0, 41.0, 42.0, 43.0]);
    let baseline_lines: Vec<String> = baseline_lines
        .iter()
        .map(|l| l.replace("\"status\":\"concluded\"", "\"status\":\"censored\""))
        .collect();
    rust_runs[0].status = Status::Concluded;
    assert!(flagged(&compare(&rust_runs, &baseline_runs(&baseline_lines))).is_empty());
}

#[test]
fn only_runs_both_benches_ran_are_compared_in_the_order_the_records_name_scenarios() {
    let (zeta_rust, zeta_baseline) = series("zeta", &[10.0, 11.0]);
    let (alpha_rust, alpha_baseline) = series("alpha", &[20.0, 21.0]);
    let mut rust_runs = zeta_rust;
    rust_runs.extend(alpha_rust);
    rust_runs.push(rust("only-rust", 1, 5.0, 1));
    let mut baseline_lines = alpha_baseline;
    baseline_lines.extend(zeta_baseline);
    baseline_lines.push(baseline_line("only-baseline", 1, 5.0, 1));
    baseline_lines.push(baseline_line("alpha", 99, 1000.0, 1));
    let lines = compare(&rust_runs, &baseline_runs(&baseline_lines));
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
            .all(|l| l.rust_runs == l.baseline_runs)
    );
    assert!(flagged(&lines).is_empty());
}

#[test]
fn nothing_shared_compares_nothing() {
    let rust_runs = [rust("a", 1, 1.0, 1)];
    assert!(compare(&rust_runs, &baseline_runs(&[baseline_line("b", 1, 1.0, 1)])).is_empty());
}

fn with_r7(line: &str, hazard: f64) -> String {
    line.replace(
        "\"partial_seconds\"",
        &format!("\"worst_r7_hazard_per_h\":{hazard},\"partial_seconds\""),
    )
}

fn find<'a>(
    lines: &'a [togi_tool_bench::sanity::Line],
    scenario: &str,
    metric: &str,
) -> &'a togi_tool_bench::sanity::Line {
    lines
        .iter()
        .find(|l| l.scenario == scenario && l.metric == metric)
        .unwrap()
}

#[test]
fn a_metric_only_rust_measured_is_a_flagged_line() {
    let (mut rust_runs, baseline_lines) = series("default", &[40.0, 41.0]);
    rust_runs[0].worst_r7_hazard_per_h = Some(1.0);
    let lines = compare(&rust_runs, &baseline_runs(&baseline_lines));
    let l = find(&lines, "default", "worst_r7_hazard_per_h");
    assert!(l.flagged);
    assert_eq!((l.rust_runs, l.baseline_runs), (1, 0));
    assert!(l.baseline.is_nan());
}

#[test]
fn a_metric_only_the_baseline_measured_is_a_flagged_line() {
    let (rust_runs, baseline_lines) = series("default", &[40.0, 41.0]);
    let baseline_lines: Vec<String> = baseline_lines.iter().map(|l| with_r7(l, 1.0)).collect();
    let lines = compare(&rust_runs, &baseline_runs(&baseline_lines));
    for scenario in ["default", "TOTAL"] {
        let l = find(&lines, scenario, "worst_r7_hazard_per_h");
        assert!(l.flagged, "{scenario}");
        assert_eq!((l.rust_runs, l.baseline_runs), (0, 2));
        assert!(l.rust.is_nan());
    }
}

#[test]
fn a_metric_measured_on_fewer_runs_is_flagged_even_at_the_same_mean() {
    let (mut rust_runs, baseline_lines) = series("default", &[40.0, 41.0, 42.0, 43.0]);
    let baseline_lines: Vec<String> = baseline_lines.iter().map(|l| with_r7(l, 1.0)).collect();
    for record in &mut rust_runs[..3] {
        record.worst_r7_hazard_per_h = Some(1.0);
    }
    let lines = compare(&rust_runs, &baseline_runs(&baseline_lines));
    let l = find(&lines, "default", "worst_r7_hazard_per_h");
    assert!(l.flagged);
    assert_eq!((l.rust_runs, l.baseline_runs), (3, 4));
    assert!((l.rust - l.baseline).abs() < f64::EPSILON);
}

#[test]
fn a_metric_neither_side_measured_is_left_out() {
    let (rust_runs, baseline_lines) = series("default", &[40.0, 41.0]);
    let lines = compare(&rust_runs, &baseline_runs(&baseline_lines));
    assert!(lines.iter().all(|l| l.metric != "worst_r7_hazard_per_h"));
    assert!(lines.iter().all(|l| l.metric != "first_passed_cycle_h"));
}

#[test]
fn a_ten_percent_shift_in_hours_is_flagged() {
    let hours = [40.0, 41.0, 42.0, 43.0, 44.0, 45.0];
    let (_, baseline_lines) = series("default", &hours);
    let shifted: Vec<Record> = hours
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust("default", seed, h * 1.1, 10))
        .collect();
    let lines = compare(&shifted, &baseline_runs(&baseline_lines));
    assert_eq!(
        flagged(&lines),
        [
            ("default".to_owned(), "sim_hours"),
            ("TOTAL".to_owned(), "sim_hours")
        ]
    );
}

#[test]
fn the_total_spread_is_within_scenarios_not_between_them() {
    let low = [9.0, 10.0, 11.0, 10.0, 10.0, 9.0, 11.0, 10.0];
    let high = low.map(|h| h + 90.0);
    let (low_rust, low_baseline) = series("low", &low);
    let (high_rust, high_baseline) = series("high", &high);
    let baseline_lines: Vec<String> = low_baseline.into_iter().chain(high_baseline).collect();
    let shifted: Vec<Record> = low_rust
        .iter()
        .chain(&high_rust)
        .map(|r| Record {
            sim_hours: r.sim_hours + 3.5,
            ..r.clone()
        })
        .collect();
    let lines = compare(&shifted, &baseline_runs(&baseline_lines));
    let total = find(&lines, "TOTAL", "sim_hours");
    assert!(total.flagged, "z {}", total.z);
    assert!(total.z > 10.0, "z {}", total.z);
    assert!(!find(&lines, "high", "sim_hours").flagged);
}

#[test]
fn the_table_marks_flagged_lines_and_counts_them() {
    let (_, baseline_lines) = series("default", &[40.0, 41.0, 42.0, 43.0]);
    let shifted: Vec<Record> = [60.0, 61.0, 62.0, 63.0]
        .iter()
        .zip(1..)
        .map(|(&h, seed)| rust("default", seed, h, 10))
        .collect();
    let lines = compare(&shifted, &baseline_runs(&baseline_lines));
    let mut out = Vec::new();
    report(&mut out, &lines).unwrap();
    insta::assert_snapshot!(String::from_utf8(out).unwrap());
}

#[test]
fn the_committed_baseline_is_the_whole_suite_without_deleted_scenarios() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/baseline.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let runs = read_baseline(&path).unwrap();
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
