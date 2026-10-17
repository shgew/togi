//! The `togi-tool-bench` command, run as a process over a fake simulator.

use std::fs;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::fake::ProcessBench as Bench;
use crate::git::{isolate_git, repo};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};

fn plain_command(bench: &Bench) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"));
    isolate_git(&mut command, &bench.path("githome"));
    command
        .current_dir(repo(bench.dir.path()))
        .arg("--suite")
        .arg(bench.path("suite.json"))
        .arg("--jobs")
        .arg("2");
    command
}

fn bench_command(bench: &Bench) -> Command {
    let mut command = plain_command(bench);
    command.arg("--split").arg("all");
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
fn real_process_a_run_prints_the_summary_and_writes_the_records() {
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
fn real_process_a_baseline_of_the_same_runs_is_neutral() {
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
fn real_process_the_baseline_out_file_is_a_baseline_the_command_accepts_and_prints_the_same_against()
 {
    let bench = Bench::new();
    let mut first = bench_command(&bench);
    with_sim(&mut first, &bench, 0);
    let output = run(first
        .arg("--out")
        .arg(bench.path("out.jsonl"))
        .arg("--baseline-out")
        .arg(bench.path("slim.jsonl")));
    assert!(output.status.success(), "{}", text(&output.stderr));
    let slim = fs::read_to_string(bench.path("slim.jsonl")).unwrap();
    assert_eq!(slim.lines().count(), 5);
    assert!(
        !slim.contains("wall_s") && !slim.contains("model_check"),
        "{slim}"
    );
    let full = fs::read_to_string(bench.path("out.jsonl")).unwrap();
    assert!(full.contains("wall_s"), "{full}");

    let against = |file: &str| {
        let mut command = bench_command(&bench);
        with_sim(&mut command, &bench, 0);
        let output = run(command.arg("--baseline").arg(bench.path(file)));
        assert!(output.status.success(), "{file}: {}", text(&output.stderr));
        text(&output.stdout)
            .lines()
            .filter(|line| !line.starts_with("harness_wall_s="))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let against_full = against("out.jsonl");
    assert!(against_full.contains("verdict: NEUTRAL"), "{against_full}");
    assert_eq!(against("slim.jsonl"), against_full);
}

#[test]
fn real_process_kept_runs_are_named_on_stderr() {
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
fn real_process_a_baseline_missing_runs_of_the_split_is_warned_about_in_the_report() {
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
fn real_process_a_missing_simulator_fails_with_the_reason() {
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
fn real_process_a_usage_error_exits_2_and_help_exits_0() {
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
fn real_process_the_cache_defaults_to_the_user_cache_directory() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    command
        .arg("--sim")
        .arg(bench.simulator("sim", 0, "", ""))
        .env("XDG_CACHE_HOME", bench.path("xdg"))
        .env_remove("HOME");
    assert!(run(&mut command).status.success());
    assert!(bench.path("xdg/togi/bench/runs").is_dir());
}

#[test]
fn real_process_the_cache_defaults_under_home_and_without_either_is_an_error() {
    let bench = Bench::new();
    let sim = bench.simulator("sim", 0, "", "");
    let mut home = bench_command(&bench);
    home.arg("--sim")
        .arg(&sim)
        .env("HOME", bench.path("home"))
        .env_remove("XDG_CACHE_HOME");
    assert!(run(&mut home).status.success());
    let subdirectory = if cfg!(target_os = "macos") {
        "home/Library/Caches"
    } else {
        "home/.cache"
    };
    assert!(bench.path(subdirectory).join("togi/bench/runs").is_dir());
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
fn real_process_no_cache_runs_every_session() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    command
        .arg("--sim")
        .arg(bench.simulator("sim", 0, "", ""))
        .arg("--no-cache");
    assert!(run(&mut command).status.success());
    assert!(text(&run(&mut command).stdout).contains("cache: 0 of 5 runs cached"));
}

fn sanity(records: &Path, baseline: &Path) -> Output {
    run(Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"))
        .arg("sanity")
        .arg("--records")
        .arg(records)
        .arg("--baseline")
        .arg(baseline))
}

#[test]
fn real_process_sanity_of_records_against_themselves_flags_nothing() {
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
fn real_process_sanity_exits_1_when_a_metric_is_flagged() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    with_sim(&mut command, &bench, 0);
    assert!(
        run(command.arg("--out").arg(bench.path("out.jsonl")))
            .status
            .success()
    );
    let censored: String = fs::read_to_string(bench.path("out.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
            value["status"] = "censored".into();
            value.to_string() + "\n"
        })
        .collect();
    fs::write(bench.path("current-censored.jsonl"), censored).unwrap();
    let output = sanity(
        &bench.path("current-censored.jsonl"),
        &bench.path("out.jsonl"),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stdout).contains("FLAGGED"));
}

#[test]
fn real_process_sanity_with_nothing_in_common_or_a_missing_file_fails() {
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

fn records_of(path: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn splits_of(records: &[serde_json::Value]) -> Vec<&str> {
    let mut splits: Vec<&str> = records
        .iter()
        .map(|record| record["split"].as_str().unwrap())
        .collect();
    splits.dedup();
    splits
}

#[test]
fn real_process_the_default_split_is_dev_and_holdout_runs_only_the_holdout_seeds() {
    let bench = Bench::new();
    let mut dev = plain_command(&bench);
    with_sim(&mut dev, &bench, 0);
    let output = run(dev.arg("--out").arg(bench.path("dev.jsonl")));
    assert!(output.status.success(), "{}", text(&output.stderr));
    let records = records_of(&bench.path("dev.jsonl"));
    assert_eq!(records.len(), 4);
    assert_eq!(splits_of(&records), ["dev"]);
    let mut holdout = plain_command(&bench);
    with_sim(&mut holdout, &bench, 0);
    let output = run(holdout
        .arg("--split")
        .arg("holdout")
        .arg("--out")
        .arg(bench.path("holdout.jsonl")));
    assert!(output.status.success(), "{}", text(&output.stderr));
    let records = records_of(&bench.path("holdout.jsonl"));
    assert_eq!(records.len(), 1);
    assert_eq!(splits_of(&records), ["holdout"]);
}

#[test]
fn real_process_the_tree_stamp_is_the_repository_the_bench_runs_in() {
    let bench = Bench::new();
    let mut command = bench_command(&bench);
    with_sim(&mut command, &bench, 0);
    assert!(
        run(command.arg("--out").arg(bench.path("out.jsonl")))
            .status
            .success()
    );
    let records = records_of(&bench.path("out.jsonl"));
    assert!(
        records.iter().all(
            |record| record["dirty"] == false && !record["commit"].as_str().unwrap().is_empty()
        )
    );
}

#[test]
fn real_process_a_records_file_in_the_tree_named_through_dots_does_not_make_it_dirty() {
    let bench = Bench::new();
    fs::create_dir(repo(bench.dir.path()).join("sub")).unwrap();
    fs::write(repo(bench.dir.path()).join("out.jsonl"), "").unwrap();
    let mut command = bench_command(&bench);
    with_sim(&mut command, &bench, 0);
    let output = run(command.arg("--out").arg("sub/../out.jsonl"));
    assert!(output.status.success(), "{}", text(&output.stderr));
    let records = records_of(&repo(bench.dir.path()).join("out.jsonl"));
    assert!(records.iter().all(|record| record["dirty"] == false));
}

#[test]
fn real_process_outside_a_git_repository_the_bench_fails_naming_it() {
    let bench = Bench::new();
    let outside = bench.path("outside");
    fs::create_dir(&outside).unwrap();
    let mut command = bench_command(&bench);
    with_sim(&mut command, &bench, 0);
    let output = run(command
        .current_dir(&outside)
        .env("GIT_CEILING_DIRECTORIES", bench.dir.path()));
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.starts_with("togi-tool-bench: git "), "{stderr}");
    assert!(stderr.contains("not a git repository"), "{stderr}");
}

#[test]
fn real_process_outside_a_git_repository_the_forecast_fails_naming_it() {
    let bench = Bench::new();
    let outside = bench.path("outside");
    fs::create_dir(&outside).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"));
    isolate_git(&mut command, &bench.path("githome"));
    let output = run(command
        .arg("forecast")
        .arg(bench.path("state"))
        .current_dir(&outside)
        .env("GIT_CEILING_DIRECTORIES", bench.dir.path()));
    assert_eq!(output.status.code(), Some(1));
    let stderr = text(&output.stderr);
    assert!(stderr.starts_with("togi-tool-bench: git "), "{stderr}");
    assert!(stderr.contains("not a git repository"), "{stderr}");
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn exit_of(child: &mut Child) -> Output {
    let mut stderr = String::new();
    wait_for("the bench to exit", || child.try_wait().unwrap().is_some());
    std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut stderr).unwrap();
    Output {
        status: child.try_wait().unwrap().unwrap(),
        stdout: Vec::new(),
        stderr: stderr.into_bytes(),
    }
}

#[test]
fn real_process_a_termination_stops_the_bench_and_its_simulators_without_blocking_their_signals() {
    let bench = Bench::new();
    let pids = bench.path("pids.txt");
    let before = format!(
        "if [ -r /proc/$$/status ]; then while read -r line; do case $line in SigBlk*) echo \"$line\";; esac; done < /proc/$$/status > '{dir}/sigblk.'$$; fi\necho $$ >> '{pids}'\nexec sleep 60",
        pids = pids.display(),
        dir = bench.dir.path().display(),
    );
    let sim = bench.simulator("sim", 0, "", &before);
    let mut command = bench_command(&bench);
    command
        .arg("--sim")
        .arg(sim)
        .arg("--cache")
        .arg(bench.path("cache"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    wait_for("both simulators", || {
        fs::read_to_string(&pids).is_ok_and(|started| started.lines().count() == 2)
    });
    let signalled = Instant::now();
    kill_process(
        Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap(),
        Signal::TERM,
    )
    .unwrap();
    let output = exit_of(&mut child);
    assert!(signalled.elapsed() < Duration::from_secs(20));
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("interrupted"));
    for line in fs::read_to_string(&pids).unwrap().lines() {
        let pid = Pid::from_raw(line.parse().unwrap()).unwrap();
        assert!(
            test_kill_process(pid).is_err(),
            "simulator {pid:?} is still running"
        );
        if cfg!(target_os = "linux") {
            let status = fs::read_to_string(bench.path(&format!("sigblk.{line}"))).unwrap();
            assert_eq!(status.trim(), "SigBlk:\t0000000000000000");
        }
    }
}
