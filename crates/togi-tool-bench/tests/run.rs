//! A bench run over a fake simulator: a script that leaves a recorded journal and exits as the test says.
#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

mod fake;
mod session;

use std::fs;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use fake::Bench;
use togi_tool_bench::{Error, Fleet, Options, Report, RunError, Selection, Status, execute};

const DAMAGE: &str = r#"printf 'broken\n' > "$4/events.jsonl"; exit 0"#;

fn report_text(report: &Report) -> String {
    let mut text = Vec::new();
    report.write(&mut text).unwrap();
    String::from_utf8(text).unwrap()
}

fn state_dirs(bench: &Bench) -> Vec<PathBuf> {
    let args = fs::read_to_string(bench.path("args.txt")).unwrap();
    args.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            words.find(|word| *word == "--state-dir")?;
            words.next().map(PathBuf::from)
        })
        .collect()
}

fn assert_gone(dirs: &[PathBuf]) {
    for dir in dirs {
        assert!(!dir.exists(), "{}", dir.display());
        let root = dir
            .ancestors()
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("togi-bench-runs-"))
            })
            .unwrap();
        assert!(!root.exists(), "{}", root.display());
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files_under(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

fn run(options: &Options) -> Report {
    execute(options, &Fleet::new()).unwrap()
}

fn without_wall(report: &Report) -> Vec<String> {
    report
        .records
        .iter()
        .map(|record| {
            let mut record = record.clone();
            record.wall_s = 0.0;
            serde_json::to_string(&record).unwrap()
        })
        .collect()
}

#[test]
fn every_run_gets_one_record_in_suite_order_stamped_with_the_tree() {
    let bench = Bench::new();
    let report = run(&bench.options(bench.simulator("sim", 0, "", "")));
    let names: Vec<_> = report
        .records
        .iter()
        .map(|r| format!("{}/{}-{}", r.scenario, r.split, r.seed))
        .collect();
    assert_eq!(
        names,
        [
            "default/dev-1",
            "default/dev-2",
            "default/dev-3",
            "default/holdout-101",
            "late/dev-1"
        ]
    );
    assert!(
        report
            .records
            .iter()
            .all(|r| r.status == Status::Concluded && r.exit_code == 0)
    );
    assert!(
        report
            .records
            .iter()
            .all(|r| r.commit == "abc1234" && !r.dirty && r.ruleset == 1)
    );
    assert!(
        report
            .records
            .iter()
            .all(|r| r.sim_hours > 0.0 && r.wall_s >= 0.0)
    );
    assert_eq!(report.cached, 0);
}

#[test]
fn the_simulator_gets_the_seed_a_state_directory_and_the_boot_cap() {
    let bench = Bench::new();
    run(&bench.options(bench.simulator("sim", 0, "", "")));
    let args = fs::read_to_string(bench.path("args.txt")).unwrap();
    let mut lines: Vec<&str> = args.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines.len(), 5);
    for line in &lines {
        assert!(
            line.contains("--state-dir ") && line.ends_with("--max-boots 7"),
            "{line}"
        );
        assert!(
            !line.contains("--machine") && !line.contains("--replay-facts"),
            "{line}"
        );
    }
    assert!(lines.iter().any(|line| line.starts_with("--seed 101 ")));
}

#[test]
fn a_replayed_scenario_names_its_machine_and_asks_for_the_facts() {
    let bench = Bench::new();
    let machine =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/machines/target-fit-0.json");
    fs::write(
        bench.path("suite.json"),
        format!(
            r#"{{"scenarios":[{{"name":"target","machine":"{}","replay":true,"dev":[5]}}]}}"#,
            machine.display()
        ),
    )
    .unwrap();
    let report = run(&bench.options(bench.simulator("sim", 0, "", "")));
    let args = fs::read_to_string(bench.path("args.txt")).unwrap();
    assert!(
        args.contains(&format!("--machine {}", machine.display())),
        "{args}"
    );
    assert!(args.contains("--replay-facts"), "{args}");
    let record = &report.records[0];
    assert_eq!(record.machine.as_deref(), machine.to_str());
    assert!(
        record
            .model_check
            .is_some_and(|check| check.eligible_groups > 0)
    );
    assert_eq!(report.checks.len(), 1);
}

#[test]
fn a_status_follows_the_exit_status_and_the_log() {
    let bench = Bench::new();
    for (exit, log, want) in [
        (0, "", Status::Concluded),
        (3, "", Status::Censored),
        (1, "sim: dead end stock: unstable", Status::DeadEnd),
        (1, "sim: failed to read file", Status::Error),
        (2, "", Status::Error),
    ] {
        let sim = bench.simulator(&format!("sim-{exit}"), exit, log, "");
        let mut options = bench.options(sim);
        options.cache = None;
        let report = run(&options);
        assert!(
            report.records.iter().all(|r| r.status == want),
            "{exit} {log}"
        );
        assert!(report.records.iter().all(|r| r.exit_code == exit));
    }
}

#[test]
fn the_records_do_not_depend_on_how_many_simulators_run_at_once() {
    let bench = Bench::new();
    let sim = bench.simulator("sim", 0, "", r#"[ "$2" = 1 ] && sleep 0.5"#);
    let mut one = bench.options(sim.clone());
    one.jobs = NonZeroUsize::MIN;
    one.cache = None;
    let mut many = one.clone();
    many.jobs = NonZeroUsize::new(5).unwrap();
    assert_eq!(without_wall(&run(&one)), without_wall(&run(&many)));
}

#[test]
fn a_second_run_over_the_same_inputs_comes_from_the_cache() {
    let bench = Bench::new();
    let options = bench.options(bench.simulator("sim", 0, "", ""));
    let first = run(&options);
    let second = run(&options);
    assert_eq!((first.cached, second.cached), (0, 5));
    assert_eq!(first.records, second.records);
    assert_eq!(
        fs::read_to_string(bench.path("args.txt"))
            .unwrap()
            .lines()
            .count(),
        5
    );
}

#[test]
fn another_simulator_binary_misses_the_cache() {
    let bench = Bench::new();
    run(&bench.options(bench.simulator("sim", 0, "", "")));
    let other = bench.simulator("other", 0, "", "true");
    assert_eq!(run(&bench.options(other)).cached, 0);
}

#[test]
fn another_boot_cap_misses_the_cache() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "", ""));
    run(&options);
    options.max_boots = NonZeroU32::new(8).unwrap();
    assert_eq!(run(&options).cached, 0);
}

#[test]
fn a_changed_suite_file_misses_the_cache() {
    let bench = Bench::new();
    let options = bench.options(bench.simulator("sim", 0, "", ""));
    run(&options);
    fs::write(
        bench.path("suite.json"),
        r#"{"scenarios":[{"name":"default","dev":[1,2,3],"holdout":[101]},{"name":"late","dev":[1],"smoke":[1]}]}"#,
    )
    .unwrap();
    assert_eq!(run(&options).cached, 0);
}

#[test]
fn a_changed_machine_file_misses_the_cache_and_another_run_hits() {
    let bench = Bench::new();
    fs::write(bench.path("m.json"), r#"{"cores": 16}"#).unwrap();
    fs::write(
        bench.path("suite.json"),
        r#"{"scenarios":[{"name":"m","machine":"m.json","dev":[1]},{"name":"default","dev":[1]}]}"#,
    )
    .unwrap();
    let options = bench.options(bench.simulator("sim", 0, "", ""));
    run(&options);
    assert_eq!(run(&options).cached, 2);
    fs::write(
        bench.path("m.json"),
        r#"{"cores": 16, "description": "changed"}"#,
    )
    .unwrap();
    assert_eq!(run(&options).cached, 1);
}

#[test]
fn keeping_the_runs_skips_the_cache_and_leaves_their_directories() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "log line", ""));
    run(&options);
    options.keep = Some(bench.path("kept"));
    let report = run(&options);
    assert_eq!(report.cached, 0);
    let kept = report.kept.unwrap();
    assert!(kept.starts_with(bench.path("kept")));
    assert!(kept.join("default/dev-1/events.jsonl").is_file());
    assert!(
        fs::read_to_string(kept.join("default/dev-1/sim.log"))
            .unwrap()
            .contains("log line")
    );
}

#[test]
fn without_keeping_no_run_directory_survives() {
    let bench = Bench::new();
    let report = run(&bench.options(bench.simulator("sim", 0, "", "")));
    assert!(report.kept.is_none());
    let dirs = state_dirs(&bench);
    assert_eq!(dirs.len(), 5);
    assert_gone(&dirs);
}

#[test]
fn a_failed_run_leaves_no_run_directory_either() {
    let bench = Bench::new();
    let record = format!(
        r#"echo "$@" >> '{}'; sleep 30"#,
        bench.path("args.txt").display()
    );
    let mut options = bench.options(bench.simulator("slow", 0, "", &record));
    options.timeout = Duration::from_secs(1);
    options.jobs = NonZeroUsize::MIN;
    options.cache = None;
    execute(&options, &Fleet::new()).unwrap_err();
    let dirs = state_dirs(&bench);
    assert_eq!(dirs.len(), 1);
    assert_gone(&dirs);
}

#[test]
fn the_records_file_is_written_and_a_baseline_is_compared() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "", ""));
    options.out = Some(bench.path("out.jsonl"));
    let first = run(&options);
    let written = togi_tool_bench::read_records(&bench.path("out.jsonl"), "records").unwrap();
    assert_eq!(written, first.records);
    options.baseline = Some(bench.path("out.jsonl"));
    let report = run(&options);
    let mut text = Vec::new();
    report.write(&mut text).unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("verdict: NEUTRAL"), "{text}");
    assert!(text.contains("cache: 5 of 5 runs cached"), "{text}");
    assert!(text.contains("harness_wall_s="));
}

#[test]
fn a_simulator_that_outlives_the_limit_is_a_harness_error_not_an_outcome() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("slow", 0, "", "sleep 30"));
    options.timeout = Duration::from_secs(1);
    options.cache = None;
    options.jobs = NonZeroUsize::MIN;
    let error = execute(&options, &Fleet::new()).unwrap_err();
    assert!(
        matches!(&error, Error::Run { source, .. } if matches!(**source, RunError::TimedOut { seconds: 1 })),
        "{error}"
    );
    assert!(error.to_string().contains("hang safety limit"), "{error}");
}

#[test]
fn a_cancelled_fleet_runs_nothing_and_reports_the_interrupt() {
    let bench = Bench::new();
    let fleet = Fleet::new();
    fleet.cancel();
    let options = bench.options(bench.simulator("sim", 0, "", ""));
    assert!(matches!(execute(&options, &fleet), Err(Error::Interrupted)));
    assert!(!bench.path("args.txt").exists());
}

#[test]
fn a_missing_or_unrunnable_simulator_is_an_error() {
    let bench = Bench::new();
    let error = execute(&bench.options(bench.path("missing")), &Fleet::new()).unwrap_err();
    assert!(error.to_string().contains("read the simulator"), "{error}");
    let plain = bench.path("plain");
    fs::write(&plain, "not a program").unwrap();
    let mut options = bench.options(plain);
    options.cache = None;
    let error = execute(&options, &Fleet::new()).unwrap_err();
    assert!(error.to_string().contains("start the simulator"), "{error}");
}

#[test]
fn a_suite_with_no_selected_runs_is_refused_before_any_simulator_runs() {
    let bench = Bench::new();
    fs::write(
        bench.path("suite.json"),
        r#"{"scenarios":[{"name":"x","holdout":[1]}]}"#,
    )
    .unwrap();
    let mut options = bench.options(bench.simulator("sim", 0, "", ""));
    options.selection = Selection::Dev;
    let error = execute(&options, &Fleet::new()).unwrap_err();
    assert!(error.to_string().contains("no selected runs"), "{error}");
    assert!(!bench.path("args.txt").exists());
}

#[test]
fn a_baseline_with_a_run_twice_is_refused() {
    let bench = Bench::new();
    let mut options = bench.options(bench.simulator("sim", 0, "", ""));
    options.out = Some(bench.path("out.jsonl"));
    run(&options);
    let text = fs::read_to_string(bench.path("out.jsonl")).unwrap();
    let first = text.lines().next().unwrap();
    fs::write(bench.path("dup.jsonl"), format!("{first}\n{first}\n")).unwrap();
    options.baseline = Some(bench.path("dup.jsonl"));
    let error = execute(&options, &Fleet::new()).unwrap_err();
    assert!(
        error.to_string().contains("duplicate baseline run"),
        "{error}"
    );
}

#[test]
fn the_report_lists_the_summary_for_each_scenario_and_the_cache_line() {
    let bench = Bench::new();
    let report = run(&bench.options(bench.simulator("sim", 0, "", "")));
    let mut text = Vec::new();
    report.write(&mut text).unwrap();
    let text = String::from_utf8(text).unwrap();
    for needle in ["default ", "late ", "TOTAL ", "cache: 0 of 5 runs cached"] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
}

#[test]
fn the_report_prints_the_suites_notes_before_the_summary_and_none_without_a_gate() {
    let bench = Bench::new();
    let options = bench.options(bench.simulator("sim", 0, "", ""));
    assert!(
        !report_text(&run(&options))
            .lines()
            .any(|line| line.starts_with("suite:"))
    );
    fs::write(
        bench.path("suite.json"),
        r#"{"gate":{"id":"g","notes":["first","second"],"baseline":{"ruleset":1,"commits":["x"]},"split":"dev","gated":["default"],"quantiles":[0.5],"confidence":0.95,"resamples":10,"bootstrap_seed":1,"max_time_ratio":2},"scenarios":[{"name":"default","dev":[1]}]}"#,
    )
    .unwrap();
    let text = report_text(&run(&options));
    assert!(text.starts_with("suite: first\nsuite: second\n"), "{text}");
    assert_eq!(text.matches("suite:").count(), 2, "{text}");
}

#[test]
fn the_simulator_that_runs_is_a_private_copy_of_the_file_that_was_hashed() {
    let bench = Bench::new();
    let original = bench.path("sim");
    let swap = format!(r"printf '#!/bin/sh\nexit 7\n' > '{}'", original.display());
    let mut options = bench.options(bench.simulator("sim", 0, "", &swap));
    options.jobs = NonZeroUsize::MIN;
    let report = run(&options);
    assert_eq!(report.records.len(), 5);
    assert!(
        report
            .records
            .iter()
            .all(|r| r.status == Status::Concluded && r.exit_code == 0)
    );
}

#[test]
fn every_simulator_leads_a_process_group_of_its_own() {
    let bench = Bench::new();
    let leader = bench.path("leader.txt");
    let probe = format!(
        r#"kill -0 "-$$" 2>/dev/null && echo leader >> '{}'"#,
        leader.display()
    );
    run(&bench.options(bench.simulator("sim", 0, "", &probe)));
    assert_eq!(fs::read_to_string(leader).unwrap().lines().count(), 5);
}

#[test]
fn a_session_killed_by_a_signal_is_an_error_record_and_is_not_cached() {
    let bench = Bench::new();
    let flag = bench.path("killed");
    let kill = format!(
        "if [ ! -e '{0}' ]; then touch '{0}'; kill -9 $$; fi",
        flag.display()
    );
    let mut options = bench.options(bench.simulator("sim", 0, "", &kill));
    options.jobs = NonZeroUsize::MIN;
    let first = run(&options);
    assert_eq!(first.records[0].status, Status::Error);
    assert_eq!(first.records[0].exit_code, -1);
    assert!(
        first.records[1..]
            .iter()
            .all(|r| r.status == Status::Concluded)
    );
    let second = run(&options);
    assert_eq!(second.cached, 4);
    assert!(second.records.iter().all(|r| r.status == Status::Concluded));
}

#[test]
fn a_journal_that_ends_in_a_signal_is_an_error_record_and_is_not_cached() {
    let bench = Bench::new();
    let flag = bench.path("stopped");
    let stop = format!(
        r#"if [ ! -e '{0}' ]; then touch '{0}'; cp '{1}' "$4/events.jsonl"; exit 0; fi"#,
        flag.display(),
        bench.path("signalled.jsonl").display()
    );
    let mut options = bench.options(bench.simulator("sim", 0, "", &stop));
    options.jobs = NonZeroUsize::MIN;
    let first = run(&options);
    assert_eq!(first.records[0].status, Status::Error);
    assert_eq!(first.records[0].exit_code, 0);
    assert!(
        first.records[1..]
            .iter()
            .all(|r| r.status == Status::Concluded)
    );
    let second = run(&options);
    assert_eq!(second.cached, 4);
    assert!(second.records.iter().all(|r| r.status == Status::Concluded));
}

#[test]
fn a_simulator_that_leaves_no_journal_is_an_error_record() {
    let bench = Bench::new();
    let none = r"echo 'sim: failed to read file'; exit 1";
    let report = run(&bench.options(bench.simulator("sim", 0, "", none)));
    assert_eq!(report.records.len(), 5);
    assert!(
        report
            .records
            .iter()
            .all(|r| r.status == Status::Error && r.exit_code == 1 && r.sim_hours == 0.0)
    );
}

#[test]
fn a_damaged_journal_is_a_harness_error() {
    let bench = Bench::new();
    let error = execute(
        &bench.options(bench.simulator("sim", 0, "", DAMAGE)),
        &Fleet::new(),
    )
    .unwrap_err();
    assert!(
        matches!(&error, Error::Run { source, .. } if matches!(**source, RunError::Journal(_))),
        "{error}"
    );
}

#[test]
fn a_failed_cache_write_is_a_warning_and_the_records_are_kept() {
    let bench = Bench::new();
    let block = format!(
        r#"for d in '{}'/runs/*; do rm -rf "$d/default"; : > "$d/default"; done"#,
        bench.path("cache").display()
    );
    let mut options = bench.options(bench.simulator("sim", 0, "", &block));
    options.jobs = NonZeroUsize::MIN;
    let report = run(&options);
    assert_eq!(report.records.len(), 5);
    assert_eq!(report.warnings.len(), 4, "{:?}", report.warnings);
    assert!(
        report.warnings[0].starts_with("write the run cache for default/dev-1: "),
        "{:?}",
        report.warnings
    );
    assert!(report.warnings[3].contains("default/holdout-101"));
    assert_eq!(run(&options).cached, 1);
}

#[test]
fn the_first_failure_stops_the_running_simulators_and_is_what_the_run_reports() {
    let bench = Bench::new();
    let failing_then_slow = format!(r#"if [ "$2" = 2 ]; then {DAMAGE}; fi; sleep 30"#);
    let mut options = bench.options(bench.simulator("sim", 0, "", &failing_then_slow));
    options.jobs = NonZeroUsize::new(2).unwrap();
    options.cache = None;
    let began = Instant::now();
    let error = execute(&options, &Fleet::new()).unwrap_err();
    assert!(began.elapsed() < Duration::from_secs(20), "{error}");
    assert!(
        matches!(&error, Error::Run { run, source } if run == "default/dev-2" && matches!(**source, RunError::Journal(_))),
        "{error}"
    );
}

#[test]
fn cancelling_while_a_simulator_runs_stops_it_and_keeps_nothing() {
    let bench = Bench::new();
    let started = bench.path("started");
    let hold = format!("touch '{}'; sleep 30", started.display());
    let mut options = bench.options(bench.simulator("slow", 0, "", &hold));
    options.keep = Some(bench.path("kept"));
    options.jobs = NonZeroUsize::MIN;
    let fleet = Fleet::new();
    let began = Instant::now();
    let result = thread::scope(|scope| {
        let running = scope.spawn(|| execute(&options, &fleet));
        let give_up = Instant::now() + Duration::from_secs(20);
        while !started.exists() && Instant::now() < give_up {
            thread::sleep(Duration::from_millis(10));
        }
        fleet.cancel();
        running.join().unwrap()
    });
    assert!(matches!(result, Err(Error::Interrupted)), "{result:?}");
    assert!(began.elapsed() < Duration::from_secs(20));
    assert!(files_under(&bench.path("cache/runs")).is_empty());
    assert_eq!(fs::read_dir(bench.path("kept")).unwrap().count(), 0);
}
