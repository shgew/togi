//! A bench run over a fake simulator: a script that leaves a recorded journal and exits as the test says.

use std::fs;
use std::num::NonZeroUsize;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::fake::ProcessBench as Bench;
use togi_tool_bench::{Error, Fleet, Options, Report, RunError, Status, execute};

const DAMAGE: &str = r#"printf 'broken\n' > "$4/events.jsonl"; exit 0"#;

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

#[test]
fn real_process_the_simulator_gets_the_seed_a_state_directory_and_the_boot_cap() {
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
fn real_process_a_replayed_scenario_names_its_machine_and_asks_for_the_facts() {
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
fn real_process_keeping_the_runs_skips_the_cache_and_leaves_their_directories() {
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
fn real_process_without_keeping_no_run_directory_survives() {
    let bench = Bench::new();
    let report = run(&bench.options(bench.simulator("sim", 0, "", "")));
    assert!(report.kept.is_none());
    let dirs = state_dirs(&bench);
    assert_eq!(dirs.len(), 5);
    assert_gone(&dirs);
}

#[test]
fn real_process_a_failed_run_leaves_no_run_directory_either() {
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
fn real_process_a_simulator_that_outlives_the_limit_is_a_harness_error_not_an_outcome() {
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
    let chain = togi_machine::error_chain(&error);
    assert!(chain.contains("hang safety limit"), "{chain}");
}

#[test]
fn real_process_a_missing_or_unrunnable_simulator_is_an_error() {
    let bench = Bench::new();
    let error = execute(&bench.options(bench.path("missing")), &Fleet::new()).unwrap_err();
    assert!(error.to_string().contains("read the simulator"), "{error}");
    let ran = bench.path("ran.txt");
    let cases = [
        ("plain", "not a program".to_owned(), 0o644),
        (
            "plain-executable",
            format!("echo ran > {}\n", ran.display()),
            0o755,
        ),
        (
            "script-with-a-blank-interpreter",
            format!("#!\necho ran > {}\n", ran.display()),
            0o755,
        ),
        (
            "script-without-the-execute-bit",
            format!("#!/bin/sh\necho ran > {}\n", ran.display()),
            0o644,
        ),
    ];
    for (name, text, mode) in cases {
        let file = bench.path(name);
        fs::write(&file, text).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(mode)).unwrap();
        let mut options = bench.options(file);
        options.cache = None;
        let error = execute(&options, &Fleet::new()).unwrap_err();
        assert!(
            error.to_string().contains("start the simulator"),
            "{name}: {error}"
        );
    }
    assert!(!ran.exists());
}

#[test]
fn real_process_the_simulator_that_runs_is_a_private_copy_of_the_file_that_was_hashed() {
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
fn real_process_every_simulator_leads_a_process_group_of_its_own() {
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
fn real_process_a_session_killed_by_a_signal_is_an_error_record_and_is_not_cached() {
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
fn real_process_a_damaged_journal_is_a_harness_error() {
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
fn real_process_the_first_failure_stops_the_running_simulators_and_is_what_the_run_reports() {
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
fn real_process_cancelling_while_a_simulator_runs_stops_it_and_keeps_nothing() {
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
