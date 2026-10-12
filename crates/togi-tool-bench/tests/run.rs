//! A bench run over a fake simulator: a script that leaves a recorded journal and exits as the test says.
#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

mod session;

use std::fs;
use std::num::{NonZeroU32, NonZeroUsize};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use togi_sim::MachineDefinition;
use togi_tool_bench::{Error, Fleet, Options, Report, RunError, Selection, Status, execute};

struct Bench {
    dir: tempfile::TempDir,
}

impl Bench {
    fn new() -> Self {
        let bench = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        let journal = session::session(&MachineDefinition::default(), 1, None, 2).events;
        let lines: Vec<String> = journal
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect();
        let lines = lines.join("\n") + "\n";
        fs::write(bench.path("journal.jsonl"), lines).unwrap();
        fs::write(
            bench.path("suite.json"),
            r#"{"scenarios":[{"name":"default","dev":[1,2,3],"holdout":[101]},{"name":"late","dev":[1]}]}"#,
        )
        .unwrap();
        bench
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// A simulator that records its arguments, copies the journal into its state directory, prints `log` and
    /// exits with `exit`; `before` runs first.
    fn simulator(&self, name: &str, exit: i32, log: &str, before: &str) -> PathBuf {
        let path = self.path(name);
        let script = format!(
            r#"#!/bin/sh
{before}
echo "$@" >> '{args}'
dir=
while [ $# -gt 0 ]; do
  if [ "$1" = --state-dir ]; then dir="$2"; fi
  shift
done
cp '{journal}' "$dir/events.jsonl"
echo '{log}'
exit {exit}
"#,
            args = self.path("args.txt").display(),
            journal = self.path("journal.jsonl").display(),
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn options(&self, simulator: PathBuf) -> Options {
        Options {
            suite: self.path("suite.json"),
            selection: Selection::All,
            out: None,
            baseline: None,
            keep: None,
            jobs: NonZeroUsize::new(2).unwrap(),
            timeout: Duration::from_secs(60),
            max_boots: NonZeroU32::new(7).unwrap(),
            cache: Some(self.path("cache")),
            simulator: Some(simulator),
        }
    }
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
            .all(|r| !r.commit.is_empty() && r.ruleset == 1)
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
    let sim = bench.simulator("sim", 0, "", "");
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
