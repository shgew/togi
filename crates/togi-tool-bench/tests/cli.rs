//! The `togi-tool-bench` command, run as a process over a fake simulator.
#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests run the built command and panic on a broken fixture"
)]

mod fake;
mod session;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use fake::Bench;

fn bench_command(bench: &Bench) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"));
    command
        .arg("--suite")
        .arg(bench.path("suite.json"))
        .arg("--split")
        .arg("all")
        .arg("--jobs")
        .arg("2");
    command
}

fn with_sim(command: &mut Command, bench: &Bench, exit: i32) {
    command
        .arg("--sim")
        .arg(bench.simulator("sim", exit, "", ""))
        .arg("--cache")
        .arg(bench.path("cache"));
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn run(command: &mut Command) -> Output {
    command.output().unwrap()
}

#[test]
fn a_run_prints_the_summary_and_writes_the_records() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    with_sim(&mut command, &bench, 0);
    let output = run(command.arg("--out").arg(bench.path("out.jsonl")));
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    for needle in ["default ", "late ", "TOTAL ", "cache: 0 of 5 runs cached"] {
        assert!(stdout.contains(needle), "{needle}: {stdout}");
    }
    assert_eq!(
        fs::read_to_string(bench.path("out.jsonl"))
            .unwrap()
            .lines()
            .count(),
        5
    );
}

#[test]
fn a_baseline_of_the_same_runs_is_neutral() {
    let bench = Bench::new();
    let mut first = bench_command(&bench);
    with_sim(&mut first, &bench, 0);
    assert!(
        run(first.arg("--out").arg(bench.path("out.jsonl")))
            .status
            .success()
    );
    let mut second = bench_command(&bench);
    with_sim(&mut second, &bench, 0);
    let output = run(second.arg("--baseline").arg(bench.path("out.jsonl")));
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.contains("verdict: NEUTRAL"), "{stdout}");
    assert!(stdout.contains("cache: 5 of 5 runs cached"), "{stdout}");
}

#[test]
fn kept_runs_are_named_on_stderr() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    with_sim(&mut command, &bench, 0);
    let output = run(command.arg("--keep").arg(bench.path("kept")));
    assert!(output.status.success(), "{}", text(&output.stderr));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("kept the runs in "), "{stderr}");
    assert_eq!(fs::read_dir(bench.path("kept")).unwrap().count(), 1);
}

#[test]
fn a_baseline_missing_runs_of_the_split_is_warned_about_in_the_report() {
    let bench = Bench::new();
    let mut first = bench_command(&bench);
    with_sim(&mut first, &bench, 0);
    assert!(
        run(first.arg("--out").arg(bench.path("out.jsonl")))
            .status
            .success()
    );
    let dev_only: String = fs::read_to_string(bench.path("out.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.contains("\"holdout\""))
        .map(|line| line.to_owned() + "\n")
        .collect();
    fs::write(bench.path("dev.jsonl"), dev_only).unwrap();
    let mut second = bench_command(&bench);
    with_sim(&mut second, &bench, 0);
    let output = run(second.arg("--baseline").arg(bench.path("dev.jsonl")));
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("WARNING 1 candidate and 0 baseline runs"));
}

#[test]
fn a_missing_simulator_fails_with_the_reason() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    let output = run(command
        .arg("--sim")
        .arg(bench.path("missing"))
        .arg("--cache")
        .arg(bench.path("cache")));
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("read the simulator"));
}

#[test]
fn a_usage_error_exits_2_and_help_exits_0() {
    let bench = Bench::new();
    let bad = run(bench_command(&bench).arg("--unknown"));
    assert_eq!(bad.status.code(), Some(2));
    let conflict = run(bench_command(&bench)
        .arg("--no-cache")
        .arg("--cache")
        .arg("x"));
    assert_eq!(conflict.status.code(), Some(2));
    let help = run(Command::new(env!("CARGO_BIN_EXE_togi-tool-bench")).arg("--help"));
    assert_eq!(help.status.code(), Some(0));
    assert!(text(&help.stdout).contains("sanity"));
}

#[test]
fn the_cache_defaults_to_the_user_cache_directory() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    command
        .arg("--sim")
        .arg(bench.simulator("sim", 0, "", ""))
        .env("XDG_CACHE_HOME", bench.path("xdg"))
        .env_remove("HOME");
    assert!(run(&mut command).status.success());
    assert!(bench.path("xdg/togi/bench-rs/runs").is_dir());
}

#[test]
fn the_cache_defaults_under_home_and_without_either_is_an_error() {
    let bench = Bench::new();
    let sim = bench.simulator("sim", 0, "", "");
    let mut home = bench_command(&bench);
    home.arg("--sim")
        .arg(&sim)
        .env("HOME", bench.path("home"))
        .env_remove("XDG_CACHE_HOME");
    assert!(run(&mut home).status.success());
    assert!(bench.path("home/.cache/togi/bench-rs/runs").is_dir());
    let mut neither = bench_command(&bench);
    neither
        .arg("--sim")
        .arg(&sim)
        .env_remove("HOME")
        .env_remove("XDG_CACHE_HOME");
    let output = run(&mut neither);
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("no cache directory"));
}

#[test]
fn no_cache_runs_every_session() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    command
        .arg("--sim")
        .arg(bench.simulator("sim", 0, "", ""))
        .arg("--no-cache");
    assert!(run(&mut command).status.success());
    assert!(text(&run(&mut command).stdout).contains("cache: 0 of 5 runs cached"));
}

fn sanity(records: &Path, go: &Path) -> Output {
    run(Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"))
        .arg("sanity")
        .arg("--records")
        .arg(records)
        .arg("--go")
        .arg(go))
}

#[test]
fn sanity_of_records_against_themselves_flags_nothing() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    with_sim(&mut command, &bench, 0);
    assert!(
        run(command.arg("--out").arg(bench.path("out.jsonl")))
            .status
            .success()
    );
    let output = sanity(&bench.path("out.jsonl"), &bench.path("out.jsonl"));
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("0 of "));
}

#[test]
fn sanity_exits_1_when_a_metric_is_flagged() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    with_sim(&mut command, &bench, 0);
    assert!(
        run(command.arg("--out").arg(bench.path("out.jsonl")))
            .status
            .success()
    );
    let doubled: String = fs::read_to_string(bench.path("out.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
            value["status"] = "censored".into();
            value.to_string() + "\n"
        })
        .collect();
    fs::write(bench.path("go.jsonl"), doubled).unwrap();
    let output = sanity(&bench.path("out.jsonl"), &bench.path("go.jsonl"));
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stdout).contains("FLAGGED"));
}

#[test]
fn sanity_with_nothing_in_common_or_a_missing_file_fails() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    with_sim(&mut command, &bench, 0);
    assert!(
        run(command.arg("--out").arg(bench.path("out.jsonl")))
            .status
            .success()
    );
    fs::write(bench.path("empty.jsonl"), "").unwrap();
    let none = sanity(&bench.path("out.jsonl"), &bench.path("empty.jsonl"));
    assert_eq!(none.status.code(), Some(1));
    assert!(text(&none.stderr).contains("no run is in both"));
    let missing = sanity(&bench.path("missing.jsonl"), &bench.path("empty.jsonl"));
    assert_eq!(missing.status.code(), Some(1));
}
