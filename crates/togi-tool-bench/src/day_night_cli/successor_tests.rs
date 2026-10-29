//! Tiny correctness runs, never a registered experiment or a policy score.
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use clap::Parser as _;
use serde_json::{Value, json};
use togi_simrun::day_night::successor::{Outcome, RunProfile};
use togi_tool_bench::day_night_successor::registration::{self, Registration};

use super::{Error, execute_plan, plan};
use crate::day_night_cli::{DayNightArgs, Profile, Split};

#[path = "../day_night_successor/registration_fixture.rs"]
mod registration_fixture;

#[derive(clap::Parser)]
struct Command {
    #[command(flatten)]
    args: DayNightArgs,
}

fn args(policy: &str) -> DayNightArgs {
    let mut argv = vec![
        "bench-day-night",
        "--registration",
        "synthetic-registration.json",
        "--split",
        "dev",
        "--out",
        "synthetic.jsonl",
        "--policy",
        policy,
        "--run-profile",
        "reduced-dev",
        "--compilation-seconds",
        "2.5",
    ];
    if policy == "ruleset13" {
        argv.extend(["--slope", "1.5"]);
    }
    Command::try_parse_from(argv).expect("CLI arguments").args
}

fn fixture() -> (Registration, Vec<u8>) {
    let mut value = registration_fixture::value(&togi_sim::day_night::successor::SCENARIO_IDS);
    value["resources"]["jobs"] = json!(1);
    value["holdout"]["procedure"] = json!("named-once-only-confirmation");
    let policies = value["sources"]["policies"]
        .as_array_mut()
        .expect("policies");
    let comparator = policies
        .iter()
        .find(|source| source["name"] == "shipped")
        .expect("shipped comparator")
        .clone();
    for name in ["co-zero", "ruleset13"] {
        let mut source = comparator.clone();
        source["name"] = json!(name);
        source["identity"] = json!(format!("synthetic-{name}"));
        policies.push(source);
    }
    let scenario = value["scenarios"]
        .as_array_mut()
        .expect("scenarios")
        .iter_mut()
        .find(|scenario| scenario["name"] == "successor-edge-deep")
        .expect("edge-deep");
    scenario["daily_mix"] = json!(
        (0..4)
            .map(|index| json!({
                "class": format!("catalog-daily-{index}"), "seconds": 7200,
            }))
            .collect::<Vec<_>>()
    );
    scenario["epochs"] = json!([{"id":"initial","start_day":1,"context":"ctx-initial","description":"synthetic epoch"}]);
    let bytes = serde_json::to_vec(&value).expect("fixture JSON");
    let mut registration = Registration::parse(&bytes).expect("strict full registration fixture");
    // The tiny seam is applied only after production validation, never accepted by parse().
    registration
        .scenarios
        .retain(|scenario| scenario.name == "successor-edge-deep");
    registration.seeds.dev.count = 1;
    registration.calendar.days = 1;
    (registration, bytes)
}

#[test_case::test_case("co-zero"; "stock_comparator")]
#[test_case::test_case("ruleset13"; "shipped_comparator")]
fn short_comparator_runs_through_cli_executor(policy: &str) {
    let (registration, bytes) = fixture();
    let plan = plan(&args(policy), &registration, &bytes).expect("tiny plan");
    let mut output = Vec::new();
    let calls = AtomicUsize::new(0);
    let manifest = execute_plan(
        plan,
        &registration,
        &|config, policy| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert!(config.deadline.is_some());
            togi_simrun::day_night::successor::run(config, policy)
        },
        &mut output,
    )
    .expect("CLI executor")
    .manifest;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(manifest.completion_count, 1);
    assert_eq!(manifest.retained_count, 1);
    assert_eq!(manifest.gate_verdict, None);
    assert_eq!(manifest.label, registration::REDUCED_DEV_LABEL);
    assert_eq!(manifest.compilation_seconds, 2.5);
    let record: Value = serde_json::from_slice(&output).expect("one JSONL record");
    assert_eq!(
        record["provenance"]["policy_identity"],
        format!("synthetic-{policy}")
    );
}

#[test]
fn expiry_inside_a_run_retains_partial_record_without_verdict() {
    let (registration, bytes) = fixture();
    let mut plan = plan(&args("co-zero"), &registration, &bytes).expect("plan");
    plan.budget = Duration::from_secs(30);
    let mut output = Vec::new();
    let execution = execute_plan(
        plan,
        &registration,
        &|mut config, policy| {
            // Deterministically expire the supplied in-run deadline, without sleeping.
            config.deadline = Some(Instant::now());
            togi_simrun::day_night::successor::run(config, policy)
        },
        &mut output,
    )
    .expect("partial artifacts");
    let manifest = &execution.manifest;
    let record: togi_simrun::day_night::successor::Record =
        serde_json::from_slice(&output).expect("partial record");
    assert!(matches!(record.outcome, Outcome::Truncated { .. }));
    assert_eq!(manifest.retained_count, 1);
    assert_eq!(manifest.completion_count, 0);
    assert_eq!(manifest.gate_verdict, None);
    let mut rendered = Vec::new();
    assert!(
        super::report_execution(&args("co-zero"), &registration, &execution, &mut rendered)
            .expect("interrupted report")
    );
    let rendered = String::from_utf8(rendered).expect("rendered interruption");
    assert!(rendered.contains("scorer skipped; no verdict"));
    assert!(!rendered.contains("# Successor day/night score"));
}

#[test]
fn expired_invocation_starts_no_runs_and_remains_incomplete() {
    let (registration, bytes) = fixture();
    let mut plan = plan(&args("co-zero"), &registration, &bytes).expect("plan");
    plan.budget = Duration::ZERO;
    let mut output = Vec::new();
    let execution = execute_plan(
        plan,
        &registration,
        &|_, _| panic!("expired invocation must not execute"),
        &mut output,
    )
    .expect("manifest");
    let manifest = &execution.manifest;
    assert!(manifest.budget_expired);
    assert_eq!(manifest.completion_count, 0);
    assert_eq!(manifest.planned_count, 1);
    assert!(output.is_empty());
    assert_eq!(manifest.gate_verdict, None);
    let mut rendered = Vec::new();
    assert!(
        super::report_execution(&args("co-zero"), &registration, &execution, &mut rendered)
            .expect("budget report")
    );
    let rendered = String::from_utf8(rendered).expect("rendered budget expiry");
    assert!(rendered.contains("Execution budget expired"));
    assert!(!rendered.contains("PASS"));
}

#[test_case::test_case(Split::Holdout; "holdout")]
#[test_case::test_case(Split::All; "all_includes_holdout")]
fn holdout_refusal_names_once_only_procedure(split: Split) {
    let (registration, bytes) = fixture();
    let mut args = args("co-zero");
    args.split = split;
    let error = match plan(&args, &registration, &bytes) {
        Ok(_) => panic!("holdout accepted"),
        Err(error) => error,
    };
    assert!(matches!(&error, Error::Holdout { .. }));
    assert!(error.to_string().contains("named-once-only-confirmation"));
}

#[test]
fn registered_profile_is_diagnostics_only() {
    let (registration, bytes) = fixture();
    let mut args = args("co-zero");
    args.run_profile = Some(Profile::Registered);
    let plan = plan(&args, &registration, &bytes).expect("registered plan");
    assert_eq!(plan.profile, RunProfile::Registered);
    let manifest = execute_plan(
        plan,
        &registration,
        &togi_simrun::day_night::successor::run,
        &mut Vec::new(),
    )
    .expect("diagnostic run")
    .manifest;
    assert_eq!(manifest.gate_verdict, None);
    assert_eq!(manifest.completion_count, 1);
}

#[test]
fn missing_profile_incompatible_slope_and_fixed_margin_are_refused() {
    let (registration, bytes) = fixture();
    let mut missing = args("co-zero");
    missing.run_profile = None;
    assert!(plan(&missing, &registration, &bytes).is_err());
    let mut slope = args("ruleset13");
    slope.slope = Some(1.7);
    assert!(plan(&slope, &registration, &bytes).is_err());
    assert!(plan(&args("fixed-margin"), &registration, &bytes).is_err());
}

#[test]
fn frozen_cli_defaults_are_unchanged() {
    let command = Command::try_parse_from(["bench-day-night"]).expect("frozen defaults");
    assert_eq!(
        command.args.registration,
        PathBuf::from("tools/bench/day-night.json")
    );
    assert!(command.args.run_profile.is_none());
    assert!(command.args.mode.is_none());
    assert!(command.args.policies.is_empty());
    assert!(command.args.out.is_none());
}

#[test]
fn registered_jobs_and_mode_cannot_be_overridden() {
    let (registration, bytes) = fixture();
    let mut jobs = args("co-zero");
    jobs.jobs = std::num::NonZeroUsize::new(2);
    assert!(plan(&jobs, &registration, &bytes).is_err());
    let mut mode = args("co-zero");
    mode.mode = Some(crate::day_night_cli::Mode::PerCore);
    assert!(plan(&mode, &registration, &bytes).is_err());
}

#[test]
fn full_production_parse_rejects_tiny_fixture_and_unknown_fields() {
    let (registration, bytes) = fixture();
    assert!(Registration::parse(&serde_json::to_vec(&registration).expect("tiny JSON")).is_err());
    let mut value: Value = serde_json::from_slice(&bytes).expect("full JSON");
    value["unregistered-field"] = json!(true);
    assert!(Registration::parse(&serde_json::to_vec(&value).expect("unknown JSON")).is_err());
}

#[test]
fn ordered_mix_and_epoch_context_must_match_truth() {
    let (mut registration, bytes) = fixture();
    registration.scenarios[0].daily_mix.swap(0, 1);
    assert!(plan(&args("co-zero"), &registration, &bytes).is_err());
    registration.scenarios[0].daily_mix.swap(0, 1);
    registration.scenarios[0].epochs[0].context = "old-context".to_owned();
    assert!(plan(&args("co-zero"), &registration, &bytes).is_err());
}

#[test]
fn pending_comparator_source_cannot_execute() {
    let (mut registration, bytes) = fixture();
    let source = registration
        .sources
        .policies
        .iter_mut()
        .find(|source| source.name == "co-zero")
        .expect("co-zero source");
    source.source = registration::SourceIdentity::Pending {};
    assert!(plan(&args("co-zero"), &registration, &bytes).is_err());
}

#[test]
fn stock_comparator_accepts_registered_evaluation_slope_without_a_policy_parameter() {
    let (registration, bytes) = fixture();
    let mut args = args("co-zero");
    args.slope = Some(2.0);
    let stock_plan = plan(&args, &registration, &bytes).expect("stock evaluation cell");
    assert_eq!(
        stock_plan.policy,
        togi_simrun::day_night::PolicyKind::CoZero
    );
    assert_eq!(stock_plan.slope, 2.0);
    assert_eq!(stock_plan.configs[0].provenance.slope, 2.0);
    args.slope = Some(1.7);
    assert!(plan(&args, &registration, &bytes).is_err());
}

#[test]
fn score_from_combines_files_without_running_or_writing_run_artifacts() {
    let (_, bytes) = fixture();
    let registration = Registration::parse(&bytes).expect("full registration");
    let args = Command::try_parse_from([
        "bench-day-night",
        "--registration",
        "synthetic-registration.json",
        "--run-profile",
        "reduced-dev",
        "--split",
        "dev",
        "--policy",
        "co-zero",
        "--score-from",
        "slope-1.5.jsonl",
        "--score-from",
        "slope-2.0.jsonl",
    ])
    .expect("score-only CLI")
    .args;
    assert!(args.out.is_none());
    let calls = AtomicUsize::new(0);
    let mut output = Vec::new();
    let incomplete = super::score_files(
        &args,
        &registration,
        &bytes,
        &|path| {
            calls.fetch_add(1, Ordering::SeqCst);
            assert!(
                path == std::path::Path::new("slope-1.5.jsonl")
                    || path == std::path::Path::new("slope-2.0.jsonl")
            );
            Ok(Vec::new())
        },
        &mut output,
    )
    .expect("synthetic missing-record score");
    assert!(incomplete);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let output = String::from_utf8(output).expect("rendered score");
    assert!(output.contains(registration::REDUCED_DEV_LABEL));
    assert!(output.contains("Slopes scored: 1.5, 2"));
    assert!(!output.contains("PASS"));
    assert!(!output.contains("Single-slope criteria only"));
}

#[test]
fn score_only_holdout_is_refused_before_reading_any_records() {
    let (registration, bytes) = fixture();
    let mut args = args("co-zero");
    args.split = Split::Holdout;
    args.out = None;
    args.score_from = vec![PathBuf::from("sealed.jsonl")];
    let error = super::score_files(
        &args,
        &registration,
        &bytes,
        &|_| panic!("holdout records must not be read"),
        &mut Vec::new(),
    )
    .expect_err("holdout refused");
    assert!(matches!(error, Error::Holdout { .. }));
}

#[test]
fn score_only_refuses_malformed_records_and_names_the_file() {
    let (registration, bytes) = fixture();
    let mut args = args("co-zero");
    args.out = None;
    args.score_from = vec![PathBuf::from("broken.jsonl")];
    let error = super::score_files(
        &args,
        &registration,
        &bytes,
        &|_| Ok(b"{broken".to_vec()),
        &mut Vec::new(),
    )
    .expect_err("bad JSONL");
    assert!(error.to_string().contains("broken.jsonl"));
}

#[test]
fn score_selection_requires_all_slopes_for_an_experiment_level_read() {
    let (registration, _) = fixture();
    let mut args = args("ruleset13");
    args.slope = None;
    args.run_profile = Some(Profile::Registered);
    let all = super::score_selection(&args, &registration).expect("all slopes");
    assert_eq!(all.slopes, None);
    args.slope = Some(1.5);
    let subset = super::score_selection(&args, &registration).expect("one slope");
    assert_eq!(subset.slopes, Some(vec![1.5]));
}

#[test]
fn registered_single_slope_renders_criteria_and_demands_other_slope_records() {
    let (_, bytes) = fixture();
    let registration = Registration::parse(&bytes).expect("full registration");
    let mut args = args("ruleset13");
    args.run_profile = Some(Profile::Registered);
    let selection = super::score_selection(&args, &registration).expect("single slope");
    let mut output = Vec::new();
    let mut result = super::render_score(
        &registration,
        &registration::sha256_hex(&bytes),
        &selection,
        &[],
        &mut output,
    )
    .expect("synthetic missing-record score");
    assert_eq!(result.verdict, super::Verdict::Incomplete);
    let output = String::from_utf8(output).expect("rendered score");
    assert!(output.contains("Single-slope criteria only: no registered gate verdict"));
    assert!(output.contains("| Criterion | Column | Rule | Measured | Result |"));
    assert!(!output.contains("PASS"));
    assert!(super::invocation_failed(&result));
    // Synthetic exit-disposition fixture: cell completeness is supplied directly, not
    // claimed as a simulated result. The remaining issue is the real scorer's slope subset.
    result.faults.clear();
    result.complete_cells = result.expected_cells;
    assert!(!super::invocation_failed(&result));
    result
        .issues
        .push("synthetic conflicting provenance".to_owned());
    assert!(super::invocation_failed(&result));
}

#[test]
fn equal_core_count_cannot_hide_a_different_ccd_membership() {
    let (mut registration, bytes) = fixture();
    registration.scenarios[0].topology.ccds = 1;
    registration.scenarios[0].topology.cores_per_ccd = 16;
    assert!(plan(&args("co-zero"), &registration, &bytes).is_err());
}
