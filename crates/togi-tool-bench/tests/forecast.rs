//! `forecast` over a fake simulator that answers each seed with the journal a real simulator wrote when it resumed
//! the same copy, so the runs, the anchor and the outcomes are the real tuner's.
#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests run the built command and panic on a broken fixture"
)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::num::{NonZeroU32, NonZeroUsize};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use sha2::{Digest as _, Sha256};
use togi_config::Config;
use togi_journal::fs::FileKind;
use togi_journal::{Build, Event, MemFs, Payload, Recording, SCHEMA, Seq, SessionId};
use togi_machine::{CancellationToken, CoreId, ObservedOffset, PerCore};
use togi_render::Renderer;
use togi_session::{ConfigSource, StopPolicy};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Observe, Observer, Persistence, Replay, SimInput, resume_simulator, simulate};
use togi_tool_bench::Fleet;
use togi_tool_bench::forecast::{ForecastError, Options, Report, execute};
use togi_tool_forecast::{Anchor, Outcome, Record, Run, Status};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

const SEEDS: [u64; 3] = [1, 2, 101];
const SUITE: &str =
    r#"{"scenarios":[{"name":"target","dev":[1,2],"holdout":[101]},{"name":"other","dev":[3]}]}"#;

fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: 0,
        evidence: EVIDENCE_EPOCH,
    }
}

fn simrun(simulator: Simulator, fs: &MemFs, observer: Observer) {
    simulate(SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: build(),
        simulator,
        persistence: Persistence::Memory(fs.clone()),
        replay: Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(1000).unwrap(),
        policy: StopPolicy {
            cycles: NonZeroU32::new(1),
            ..StopPolicy::default()
        },
        write_samples: false,
        defects: &[],
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: Some(observer),
        dump_to: None,
    })
    .unwrap();
}

fn copy(fs: &MemFs) -> MemFs {
    let copy = MemFs::new();
    for (path, kind) in fs.entries() {
        if kind == FileKind::File {
            copy.put(&path, fs.contents(&path).unwrap());
        }
    }
    copy
}

fn on_disk(fs: &MemFs) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "writing a simulation's files to disk is the point"
    )]
    fs.dump_to(dir.path()).unwrap();
    dir
}

fn events(path: &Path) -> Vec<Event> {
    Recording::read(path).unwrap().into_events()
}

fn crashes_after(events: &[Event], seq: Seq) -> u32 {
    let n = events
        .iter()
        .filter(|event| event.seq > seq && matches!(event.payload, Payload::CrashDetected(_)))
        .count();
    u32::try_from(n).unwrap()
}

/// A simulated state directory whose session stopped after a crash, and the journals the same copy gets when
/// each seed of the ensemble resumes it.
struct Fixture {
    dir: tempfile::TempDir,
    anchor: Anchor,
}

impl Fixture {
    /// With `in_flight`, the copy ends at the first trial's start after the crash, as if the machine lost power
    /// there; otherwise at the crash.
    fn new(in_flight: bool) -> Self {
        let history = MemFs::new();
        let simulator = Simulator::new(
            Model::new(&MachineDefinition::default(), 42).unwrap(),
            RunSettings {
                seed: 42,
                ..RunSettings::default()
            },
        );
        let mut crashed = false;
        let stopped_at = Rc::new(Cell::new(None));
        let at = Rc::clone(&stopped_at);
        simrun(
            simulator,
            &history,
            Box::new(move |event| {
                crashed |= matches!(event.payload, Payload::CrashDetected(_));
                let stop = if in_flight {
                    crashed && matches!(event.payload, Payload::TrialStart(_))
                } else {
                    crashed
                };
                if stop {
                    at.set(Some(event.seq));
                    Observe::Stop
                } else {
                    Observe::Continue
                }
            }),
        );
        let stopped = on_disk(&history);
        if in_flight {
            let journal = stopped.path().join("state/events.jsonl");
            let keep = events(&journal)
                .iter()
                .position(|event| Some(event.seq) == stopped_at.get())
                .unwrap()
                + 1;
            let text = fs::read_to_string(&journal).unwrap();
            let lines: Vec<&str> = text.split_inclusive('\n').take(keep).collect();
            fs::write(&journal, lines.concat()).unwrap();
        }
        let fixture = Self {
            dir: tempfile::tempdir().unwrap(),
            anchor: Anchor::of(stopped.path().join("state")).unwrap(),
        };
        copy_dir(&stopped.path().join("state"), &fixture.state());
        fs::create_dir_all(fixture.fulls()).unwrap();
        for seed in SEEDS {
            let resumed = copy(&history);
            let model = Model::new(&MachineDefinition::default(), seed).unwrap();
            let simulator =
                resume_simulator(&Persistence::Memory(resumed.clone()), model, seed, None).unwrap();
            simrun(simulator, &resumed, Box::new(|_| Observe::Continue));
            let dir = on_disk(&resumed);
            fs::copy(
                dir.path().join("state/events.jsonl"),
                fixture.fulls().join(format!("{seed}.jsonl")),
            )
            .unwrap();
        }
        fs::write(fixture.path("suite.json"), SUITE).unwrap();
        fixture
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn state(&self) -> PathBuf {
        self.path("copy")
    }

    fn fulls(&self) -> PathBuf {
        self.path("fulls")
    }

    /// A simulator that gives the run of seed `seed` the journal that seed's real resume wrote, after `extra`.
    fn simulator(&self, extra: &str, exit: i32) -> PathBuf {
        let path = self.path("sim");
        let script = format!(
            r#"#!/bin/sh
seed=
dir=
while [ $# -gt 0 ]; do
  if [ "$1" = --seed ]; then seed="$2"; fi
  if [ "$1" = --state-dir ]; then dir="$2"; fi
  shift
done
{extra}
cp '{fulls}'/"$seed".jsonl "$dir/events.jsonl"
exit {exit}
"#,
            fulls = self.fulls().display(),
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn options(&self, simulator: PathBuf) -> Options {
        Options {
            state: self.state(),
            suite: self.path("suite.json"),
            out: None,
            keep: None,
            jobs: NonZeroUsize::new(3).unwrap(),
            timeout: Duration::from_secs(60),
            max_boots: NonZeroU32::new(1000).unwrap(),
            simulator: Some(simulator),
        }
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(dir, &mut files);
    files
}

fn written(report: &Report) -> (Vec<u8>, String) {
    let mut record = Vec::new();
    report.record.write(&mut record).unwrap();
    let mut table = Vec::new();
    report.write(&mut table).unwrap();
    (record, String::from_utf8(table).unwrap())
}

#[test]
fn a_forecast_counts_only_what_follows_the_anchor_and_leaves_the_copy_alone() {
    for in_flight in [false, true] {
        let fixture = Fixture::new(in_flight);
        let before = snapshot(&fixture.state());
        let copied = events(&fixture.state().join("events.jsonl"));
        if in_flight {
            let last = copied.last().unwrap();
            assert!(matches!(last.payload, Payload::TrialStart(_)));
            assert_eq!(last.seq, fixture.anchor.seq);
        }
        let earlier = crashes_after(&copied, Seq::new(1).unwrap());
        assert!(
            earlier > 0,
            "the copy has a crash before the anchor to exclude"
        );
        let mut options = fixture.options(fixture.simulator("", 0));
        options.out = Some(fixture.path("out/forecast.json"));
        let first = execute(&options, &Fleet::new()).unwrap();
        let second = execute(&options, &Fleet::new()).unwrap();
        assert_eq!(
            written(&first),
            written(&second),
            "identical forecasts differ"
        );
        assert_eq!(first.record.anchor, fixture.anchor);
        assert_eq!(first.record.summary.runs, 3);
        assert_eq!(first.record.summary.concluded, 3);
        assert!(first.kept.is_none());
        for (run, seed) in first
            .record
            .runs
            .iter()
            .zip(["dev", "dev", "holdout"].into_iter().zip(SEEDS))
        {
            assert_eq!((run.split.as_str(), run.seed), seed);
            let full = events(&fixture.fulls().join(format!("{}.jsonl", run.seed)));
            assert_eq!(run.outcome.status, Status::Concluded);
            assert_eq!(
                run.outcome.crashes,
                crashes_after(&full, fixture.anchor.seq)
            );
        }
        assert_eq!(
            Record::read(fixture.path("out/forecast.json")).unwrap(),
            first.record
        );
        assert_eq!(
            before,
            snapshot(&fixture.state()),
            "the forecast changed its input"
        );
    }
}

#[test]
fn a_run_that_fails_or_does_not_conclude_fails_the_forecast() {
    let fixture = Fixture::new(false);
    for (name, extra, exit) in [
        ("exit 2", "", 2),
        ("killed", "kill -9 $$", 0),
        ("no events after the anchor", "exit 0", 0),
        ("exit 1 without a dead end of its own", "", 1),
    ] {
        let failed = execute(
            &fixture.options(fixture.simulator(extra, exit)),
            &Fleet::new(),
        )
        .unwrap_err();
        assert!(
            matches!(*failed.error, ForecastError::Runs(_)),
            "{name}: {failed}"
        );
        assert!(
            failed.to_string().contains("forecast target/dev-1:"),
            "{name}: {failed}"
        );
    }
}

#[test]
fn exit_one_is_a_dead_end_only_when_this_run_reached_one() {
    let fixture = Fixture::new(false);
    let report = execute(
        &fixture.options(fixture.simulator("echo 'sim: dead end thermal trip: 95 C'", 1)),
        &Fleet::new(),
    )
    .unwrap();
    assert_eq!(report.record.summary.dead_ends, 3);
    assert!(
        report
            .record
            .runs
            .iter()
            .all(|run| run.outcome.status == Status::DeadEnd)
    );
}

#[test]
fn only_the_files_a_resumed_session_reads_are_copied() {
    let fixture = Fixture::new(false);
    let state = fixture.state();
    fs::create_dir_all(state.join("archive")).unwrap();
    for name in [
        "20260101T000000Z.jsonl",
        "20260102T000000Z-carry-pending",
        "20260102T000000Z-compat-pending",
        "20260101T000000Z-reset-all",
        "notes",
    ] {
        fs::write(state.join("archive").join(name), "x\n").unwrap();
    }
    fs::create_dir_all(state.join("trials/t")).unwrap();
    fs::write(state.join("trials/t/samples.jsonl.gz"), "x").unwrap();
    let listing = fixture.path("listing");
    let extra = format!(
        "(cd \"$dir\" && find . -type f | LC_ALL=C sort) > '{}'",
        listing.display()
    );
    let mut options = fixture.options(fixture.simulator(&extra, 0));
    options.jobs = NonZeroUsize::MIN;
    let _ = execute(&options, &Fleet::new());
    assert_eq!(
        fs::read_to_string(listing).unwrap(),
        "./archive/20260101T000000Z-reset-all\n\
         ./archive/20260101T000000Z.jsonl\n\
         ./archive/20260102T000000Z-carry-pending\n\
         ./archive/20260102T000000Z-compat-pending\n\
         ./events.jsonl\n\
         ./sim.log\n\
         ./state.json\n"
    );
}

#[test]
fn an_archive_entry_that_is_not_a_regular_file_fails_naming_it() {
    for name in [
        "20260101T000000Z.jsonl",
        "20260101T000000Z-carry-pending",
        "20260101T000000Z-compat-pending",
        "20260101T000000Z-reset-all",
    ] {
        let fixture = Fixture::new(false);
        fs::create_dir_all(fixture.state().join("archive")).unwrap();
        let target = fixture.path("elsewhere");
        fs::write(&target, "archived\n").unwrap();
        std::os::unix::fs::symlink(&target, fixture.state().join("archive").join(name)).unwrap();
        let failed =
            execute(&fixture.options(fixture.simulator("", 0)), &Fleet::new()).unwrap_err();
        assert!(failed.to_string().contains(name), "{name}: {failed}");
    }
}

fn run_dirs(root: &Path) -> Vec<String> {
    let mut dirs = Vec::new();
    let Ok(scenarios) = fs::read_dir(root) else {
        return dirs;
    };
    for scenario in scenarios {
        let scenario = scenario.unwrap().path();
        for run in fs::read_dir(&scenario).unwrap() {
            let run = run.unwrap().path();
            dirs.push(format!(
                "{}/{}",
                scenario.file_name().unwrap().to_string_lossy(),
                run.file_name().unwrap().to_string_lossy()
            ));
        }
    }
    dirs.sort();
    dirs
}

#[test]
fn a_failed_forecast_keeps_only_the_failed_runs_and_says_where() {
    let fixture = Fixture::new(false);
    let failed = execute(
        &fixture.options(fixture.simulator("[ \"$seed\" = 2 ] && exit 2", 0)),
        &Fleet::new(),
    )
    .unwrap_err();
    let kept = failed.kept.clone().unwrap();
    assert_eq!(run_dirs(&kept), ["target/dev-2"]);
    fs::remove_dir_all(kept).unwrap();
}

#[test]
fn a_forecast_that_fails_after_every_run_succeeded_keeps_nothing() {
    let fixture = Fixture::new(false);
    let blocker = fixture.path("file");
    fs::write(&blocker, "").unwrap();
    let mut options = fixture.options(fixture.simulator("", 0));
    options.out = Some(blocker.join("forecast.json"));
    let failed = execute(&options, &Fleet::new()).unwrap_err();
    assert!(
        matches!(*failed.error, ForecastError::Write { .. }),
        "{failed}"
    );
    assert!(failed.kept.is_none());
}

#[test]
fn an_interrupt_stops_starting_runs_and_fails_as_interrupted() {
    let fixture = Fixture::new(false);
    let fleet = Fleet::new();
    fleet.cancel();
    let mut options = fixture.options(fixture.simulator("", 0));
    options.jobs = NonZeroUsize::MIN;
    let failed = execute(&options, &fleet).unwrap_err();
    assert!(
        matches!(*failed.error, ForecastError::Interrupted),
        "{failed}"
    );
    assert!(failed.kept.is_none());
    options.keep = Some(fixture.path("kept"));
    let failed = execute(&options, &fleet).unwrap_err();
    let kept = failed.kept.unwrap();
    assert_eq!(
        run_dirs(&kept),
        ["target/dev-1"],
        "runs copied state after the interrupt"
    );
}

#[test]
fn keep_retains_every_run_directory() {
    let fixture = Fixture::new(false);
    let mut options = fixture.options(fixture.simulator("", 0));
    options.keep = Some(fixture.path("kept"));
    let report = execute(&options, &Fleet::new()).unwrap();
    assert_eq!(
        run_dirs(&report.kept.unwrap()),
        ["target/dev-1", "target/dev-2", "target/holdout-101"]
    );
}

#[test]
fn a_suite_without_the_target_scenario_has_no_ensemble() {
    let fixture = Fixture::new(false);
    fs::write(
        fixture.path("suite.json"),
        r#"{"scenarios":[{"name":"other","dev":[3]}]}"#,
    )
    .unwrap();
    let failed = execute(&fixture.options(fixture.simulator("", 0)), &Fleet::new()).unwrap_err();
    assert!(
        matches!(*failed.error, ForecastError::NoEnsemble { .. }),
        "{failed}"
    );
}

#[test]
fn a_state_directory_without_a_journal_has_no_anchor() {
    let fixture = Fixture::new(false);
    let empty = tempfile::tempdir().unwrap();
    let mut options = fixture.options(fixture.simulator("", 0));
    options.state = empty.path().to_path_buf();
    let failed = execute(&options, &Fleet::new()).unwrap_err();
    assert!(failed.to_string().starts_with("read anchor:"), "{failed}");
}

#[test]
fn the_record_lists_each_machine_file_the_ensemble_reads_once_by_suite_relative_path() {
    let fixture = Fixture::new(false);
    let machines = fixture.path("machines");
    fs::create_dir_all(&machines).unwrap();
    let contents = [
        ("big.json", r#"{"cores":8}"#),
        ("small.json", r#"{"cores":4}"#),
        ("unused.json", r#"{"cores":2}"#),
    ];
    for (name, content) in contents {
        fs::write(machines.join(name), content).unwrap();
    }
    fs::write(
        fixture.path("suite.json"),
        r#"{"scenarios":[{"name":"target","machines":["machines/big.json","machines/small.json"],"dev":[1,2],"holdout":[101]},{"name":"other","machine":"machines/unused.json","dev":[3]}]}"#,
    )
    .unwrap();
    let report = execute(&fixture.options(fixture.simulator("", 0)), &Fleet::new()).unwrap();
    let sum = |name: &str| {
        let content = contents.iter().find(|(n, _)| *n == name).unwrap().1;
        Sha256::digest(content)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                write!(hex, "{byte:02x}").unwrap();
                hex
            })
    };
    let files: Vec<(&str, String)> = report
        .record
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.sha256.clone()))
        .collect();
    assert_eq!(
        files,
        [
            ("machines/big.json", sum("big.json")),
            ("machines/small.json", sum("small.json")),
        ]
    );
    let machines: Vec<&str> = report
        .record
        .runs
        .iter()
        .map(|run| run.machine.as_str())
        .collect();
    assert_eq!(
        machines,
        [
            "machines/big.json",
            "machines/small.json",
            "machines/big.json"
        ]
    );
}

fn literal_run(
    split: &str,
    seed: u64,
    hours: f64,
    crashes: u32,
    hunts: u32,
    offsets: [i64; 2],
) -> Run {
    let profile: PerCore<ObservedOffset> = offsets
        .into_iter()
        .enumerate()
        .map(|(core, offset)| {
            (
                CoreId::new(i64::try_from(core).unwrap()).unwrap(),
                ObservedOffset(offset),
            )
        })
        .collect();
    Run {
        machine: "machines/target.json".to_owned(),
        seed,
        split: split.to_owned(),
        outcome: Outcome {
            status: Status::Concluded,
            hours,
            crashes,
            hunts,
            profile,
            depth: offsets.iter().sum(),
        },
    }
}

#[test]
fn the_report_names_the_anchor_the_runs_by_status_and_each_metric_s_range() {
    let anchor = Anchor {
        session: "20260101T000000Z".parse::<SessionId>().unwrap(),
        seq: Seq::new(2542).unwrap(),
        time: "2026-01-01T06:07:19Z".parse::<Timestamp>().unwrap(),
        sha256: "0".repeat(64),
    };
    let mut runs = vec![
        literal_run("dev", 1, 23.16, 12, 5, [-34, -9]),
        literal_run("dev", 2, 23.56, 12, 4, [-19, -2]),
        literal_run("holdout", 101, 23.76, 15, 2, [-17, -6]),
    ];
    runs[2].outcome.status = Status::Censored;
    let mut record = Record::new(anchor, "0123abc", true, Vec::new(), runs).unwrap();
    record.ruleset = 10;
    let report = Report {
        record,
        checks: Vec::new(),
        kept: None,
    };
    let mut out = Vec::new();
    report.write(&mut out).unwrap();
    insta::assert_snapshot!("the_forecast_report", String::from_utf8(out).unwrap());
}

fn command(fixture: &Fixture) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"));
    command
        .arg("forecast")
        .arg(fixture.state())
        .arg("--suite")
        .arg(fixture.path("suite.json"));
    command
}

#[test]
fn the_command_prints_the_forecast_and_writes_the_record() {
    let fixture = Fixture::new(false);
    let out = fixture.path("forecast.json");
    let output = command(&fixture)
        .arg("--sim")
        .arg(fixture.simulator("", 0))
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.starts_with("forecast: after session "), "{stdout}");
    assert!(stdout.contains("\nmetric   median"), "{stdout}");
    assert_eq!(Record::read(out).unwrap().runs.len(), 3);
}

#[test]
fn bench_flags_that_judge_a_run_are_usage_errors_with_forecast() {
    for flag in ["--split=all", "--baseline=b.jsonl", "--same=."] {
        let output = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"))
            .args(["forecast", ".", flag])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{flag}");
        assert!(output.stdout.is_empty(), "{flag}");
    }
}
