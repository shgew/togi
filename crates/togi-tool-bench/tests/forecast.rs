//! `forecast` over a fake simulator that answers each seed with the journal a real simulator wrote when it resumed
//! the same copy, so the runs, the anchor and the outcomes are the real tuner's.
#![expect(
    clippy::unwrap_used,
    reason = "the tests run the built command and panic on a broken fixture"
)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::num::{NonZeroU32, NonZeroUsize};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

#[expect(
    dead_code,
    unused_imports,
    reason = "only the compiled snapshot helper is used in this test binary"
)]
mod common;
mod git;

use git::{isolate_git, repo};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::fs::FileKind;
use togi_journal::{
    Build, DeadEnd, DeadEndCondition, Event, MemFs, Payload, SCHEMA, Seq, SessionId, Shutdown,
    ShutdownReason, Stamp, StateDir,
};
use togi_machine::{BootId, CancellationToken, CoreId, ObservedOffset, PerCore};
use togi_render::Renderer;
use togi_session::{ConfigSource, StopPolicy};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Observe, Observer, Persistence, Replay, SimInput, resume_simulator, simulate};
use togi_tool_bench::forecast::{ForecastError, Options, Report, execute_unsynced, outcome};
use togi_tool_bench::{Fleet, Tree};
use togi_tool_forecast::{Anchor, Outcome, Record, Run, Session, Status, read_dir_fs};
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
    for (path, kind) in fs.entries() {
        if kind == FileKind::File {
            let target = dir.path().join(path.strip_prefix("/").unwrap());
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(target, fs.contents(&path).unwrap()).unwrap();
        }
    }
    dir
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
    history: MemFs,
    anchor: Anchor,
    fulls: BTreeMap<u64, Vec<u8>>,
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
        if in_flight {
            let journal = history.contents("/state/events.jsonl").unwrap();
            let parsed = Session::decode(Path::new("events.jsonl"), &journal)
                .unwrap()
                .unwrap();
            let keep = parsed
                .events()
                .iter()
                .position(|event| Some(event.seq) == stopped_at.get())
                .unwrap()
                + 1;
            let text = String::from_utf8(journal).unwrap();
            let cut: String = text.split_inclusive('\n').take(keep).collect();
            history.put("/state/events.jsonl", cut);
        }
        let anchor = Anchor::of_fs(&history, "/state").unwrap();
        let mut fulls = BTreeMap::new();
        for seed in SEEDS {
            let resumed = copy(&history);
            let model = Model::new(&MachineDefinition::default(), seed).unwrap();
            let simulator =
                resume_simulator(&Persistence::Memory(resumed.clone()), model, seed, None).unwrap();
            simrun(simulator, &resumed, Box::new(|_| Observe::Continue));
            let bytes = resumed.contents("/state/events.jsonl").unwrap();
            let full = Session::decode(Path::new("events.jsonl"), &bytes)
                .unwrap()
                .unwrap();
            let full = full.events();
            let copied = Session::decode(
                Path::new("events.jsonl"),
                &history.contents("/state/events.jsonl").unwrap(),
            )
            .unwrap()
            .unwrap()
            .events()
            .len();
            if in_flight {
                assert!(
                    !matches!(
                        full[copied].payload,
                        Payload::SmuIntent(_) | Payload::TrialEnd(_) | Payload::Shutdown(_)
                    ),
                    "seed {seed} resumed a copy that was cut short, not the one the forecast gets"
                );
            }
            fulls.insert(seed, bytes);
        }
        Self {
            history,
            anchor,
            fulls,
        }
    }

    fn sessions(&self, seed: u64) -> Vec<Session> {
        let fs = copy(&self.history);
        fs.put("/state/events.jsonl", self.fulls[&seed].clone());
        read_dir_fs(&fs, "/state", Some(self.anchor.session)).unwrap()
    }

    fn cut_sessions(&self, seed: u64) -> Vec<Session> {
        let copied = Session::decode(
            Path::new("events.jsonl"),
            &self.history.contents("/state/events.jsonl").unwrap(),
        )
        .unwrap()
        .unwrap()
        .events()
        .len();
        let bytes = String::from_utf8(self.fulls[&seed].clone()).unwrap();
        let cut: String = bytes.split_inclusive('\n').take(copied + 3).collect();
        vec![
            Session::decode(Path::new("events.jsonl"), cut.as_bytes())
                .unwrap()
                .unwrap(),
        ]
    }

    fn report(&self) -> Report {
        let runs = SEEDS
            .into_iter()
            .map(|seed| Run {
                machine: String::new(),
                seed,
                split: if seed == 101 { "holdout" } else { "dev" }.to_owned(),
                outcome: outcome(&self.sessions(seed), &self.anchor, 0, false).unwrap(),
            })
            .collect();
        Report {
            record: Record::new(self.anchor.clone(), "0123abc", false, Vec::new(), runs).unwrap(),
            checks: Vec::new(),
            kept: None,
        }
    }
}

struct DiskFixture {
    dir: tempfile::TempDir,
}

impl DiskFixture {
    fn new(in_flight: bool) -> Self {
        let fixture = Fixture::new(in_flight);
        let dir = on_disk(&fixture.history);
        let disk = Self { dir };
        fs::rename(disk.path("state"), disk.state()).unwrap();
        fs::create_dir_all(disk.fulls()).unwrap();
        for (seed, bytes) in fixture.fulls {
            fs::write(disk.fulls().join(format!("{seed}.jsonl")), bytes).unwrap();
        }
        fs::write(disk.path("suite.json"), SUITE).unwrap();
        disk
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
        self.simulator_from(&self.fulls(), extra, exit)
    }

    fn simulator_from(&self, journals: &Path, extra: &str, exit: i32) -> PathBuf {
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
            fulls = journals.display(),
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn options(&self, simulator: PathBuf) -> Options {
        Options {
            state: self.state(),
            suite: self.path("suite.json"),
            tree: Tree {
                commit: "0123abc".to_owned(),
                dirty: false,
            },
            out: None,
            keep: None,
            jobs: NonZeroUsize::new(3).unwrap(),
            timeout: Duration::from_secs(60),
            max_boots: NonZeroU32::new(1000).unwrap(),
            simulator: Some(simulator),
        }
    }
}

fn written(report: &Report) -> (Vec<u8>, String) {
    let mut record = Vec::new();
    report.record.write(&mut record).unwrap();
    let mut table = Vec::new();
    report.write(&mut table).unwrap();
    (record, String::from_utf8(table).unwrap())
}

#[expect(
    clippy::disallowed_methods,
    reason = "disposable forecast process fixtures must never sync record output"
)]
fn execute(options: &Options, fleet: &Fleet) -> Result<Report, togi_tool_bench::forecast::Failed> {
    execute_unsynced(options, fleet)
}

#[test]
fn a_forecast_counts_only_what_follows_the_anchor_and_leaves_the_copy_alone() {
    for in_flight in [false, true] {
        let fixture = Fixture::new(in_flight);
        let before: Vec<_> = fixture
            .history
            .entries()
            .into_iter()
            .map(|(path, kind)| {
                let bytes = fixture.history.contents(&path);
                (path, kind, bytes)
            })
            .collect();
        let copied = Session::decode(
            Path::new("events.jsonl"),
            &fixture.history.contents("/state/events.jsonl").unwrap(),
        )
        .unwrap()
        .unwrap();
        if in_flight {
            let last = copied.events().last().unwrap();
            assert!(matches!(last.payload, Payload::TrialStart(_)));
            assert_eq!(last.seq, fixture.anchor.seq);
        }
        assert!(crashes_after(copied.events(), Seq::new(1).unwrap()) > 0);
        let first = fixture.report();
        let second = fixture.report();
        assert_eq!(written(&first), written(&second));
        assert_eq!(first.record.anchor, fixture.anchor);
        assert_eq!(first.record.summary.runs, 3);
        assert_eq!(first.record.summary.concluded, 3);
        for (run, seed) in first
            .record
            .runs
            .iter()
            .zip(["dev", "dev", "holdout"].into_iter().zip(SEEDS))
        {
            assert_eq!((run.split.as_str(), run.seed), seed);
            assert_eq!(run.outcome.status, Status::Concluded);
            assert_eq!(
                run.outcome.crashes,
                crashes_after(fixture.sessions(run.seed)[0].events(), fixture.anchor.seq)
            );
        }
        let (record, _) = written(&first);
        assert_eq!(
            Record::decode(record.as_slice(), "forecast.json").unwrap(),
            first.record
        );
        let after: Vec<_> = fixture
            .history
            .entries()
            .into_iter()
            .map(|(path, kind)| {
                let bytes = fixture.history.contents(&path);
                (path, kind, bytes)
            })
            .collect();
        assert_eq!(before, after, "the forecast changed its input");
    }
}

#[test]
fn real_process_a_run_that_fails_or_is_killed_fails_the_forecast() {
    let fixture = DiskFixture::new(false);
    for (name, extra, exit) in [("exit 2", "", 2), ("killed", "kill -9 $$", 0)] {
        let failed = execute(
            &fixture.options(fixture.simulator(extra, exit)),
            &Fleet::new(),
        )
        .unwrap_err();
        assert!(
            matches!(*failed.error, ForecastError::Runs(_)),
            "{name}: {failed}"
        );
        let message = failed.to_string();
        assert!(message.contains("forecast target/"), "{name}: {message}");
        assert!(!message.contains("interrupted"), "{name}: {message}");
    }
}

#[test]
fn exit_one_is_a_dead_end_only_when_this_run_reached_one() {
    let fixture = Fixture::new(false);
    let mut report = fixture.report();
    for run in &mut report.record.runs {
        run.outcome = outcome(&fixture.sessions(run.seed), &fixture.anchor, 1, true).unwrap();
    }
    report.record.summary = togi_tool_forecast::summarize(&report.record.runs).unwrap();
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
fn a_dead_end_the_copy_already_holds_is_not_the_runs_own() {
    let fixture = Fixture::new(false);
    #[expect(
        clippy::disallowed_methods,
        reason = "in-memory fixture exercises journal ordering without disk sync"
    )]
    let mut journal = StateDir::lock_with_fs(
        std::sync::Arc::new(fixture.history.clone()),
        Path::new("/state"),
    )
    .unwrap()
    .open_at(
        BootId::from("dead-end-fixture".to_owned()),
        "2030-01-01T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let stamp = Stamp {
        time: "2030-01-01T00:00:01Z".parse().unwrap(),
        mono: None,
    };
    let dead_end = journal
        .append(
            stamp,
            Payload::DeadEnd(DeadEnd {
                condition: DeadEndCondition::ThermalTrip,
                core: None,
                detail: "thermal trip".to_owned(),
                action: None,
            }),
            &[],
        )
        .unwrap()
        .seq;
    journal
        .append(
            stamp,
            Payload::Shutdown(Shutdown {
                reason: ShutdownReason::DeadEnd,
                kernel_cursor: None,
                kernel_error: None,
                cycles: None,
            }),
            &[dead_end],
        )
        .unwrap();
    journal.close().unwrap();
    let anchor = Anchor::of_fs(&fixture.history, "/state").unwrap();
    assert!(anchor.seq > dead_end, "the anchor follows the dead end");
    let sessions = read_dir_fs(&fixture.history, "/state", Some(anchor.session)).unwrap();
    let failed = outcome(&sessions, &anchor, 1, false).unwrap_err();
    assert!(
        matches!(failed, ForecastError::SimulatorFailed { exit: 1 }),
        "{failed}"
    );
    assert!(
        failed.to_string().contains("simulator failed with exit 1"),
        "{failed}"
    );
}

#[test]
fn real_process_only_the_files_a_resumed_session_reads_are_copied() {
    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                files.extend(snapshot(&entry.path()));
            } else {
                files.insert(entry.path(), fs::read(entry.path()).unwrap());
            }
        }
        files
    }
    let fixture = DiskFixture::new(false);
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
    let before = snapshot(&state);
    let listing = fixture.path("listing");
    let extra = format!(
        "(cd \"$dir\" && find . -type f | LC_ALL=C sort) > '{}'",
        listing.display()
    );
    let mut options = fixture.options(fixture.simulator(&extra, 0));
    options.jobs = NonZeroUsize::MIN;
    let _ = execute(&options, &Fleet::new());
    assert_eq!(
        before,
        snapshot(&state),
        "copying a run must leave every input file unchanged"
    );
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
fn real_fs_an_archive_entry_that_is_not_a_regular_file_fails_the_forecast_naming_it() {
    let name = "20260101T000000Z-reset-all";
    let fixture = DiskFixture::new(false);
    fs::create_dir_all(fixture.state().join("archive")).unwrap();
    let target = fixture.path("elsewhere");
    fs::write(&target, "archived\n").unwrap();
    std::os::unix::fs::symlink(&target, fixture.state().join("archive").join(name)).unwrap();
    let failed = execute(&fixture.options(fixture.simulator("", 0)), &Fleet::new()).unwrap_err();
    let message = togi_machine::error_chain(&failed);
    assert!(message.contains(name), "{message}");
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
fn real_process_a_failed_forecast_keeps_only_the_failed_runs_and_says_where() {
    let fixture = DiskFixture::new(false);
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
fn real_process_a_forecast_that_fails_after_every_run_succeeded_keeps_nothing() {
    let fixture = DiskFixture::new(false);
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
fn real_fs_an_interrupt_stops_starting_runs_and_fails_as_interrupted() {
    let fixture = DiskFixture::new(false);
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
fn real_process_keep_retains_every_run_directory() {
    let fixture = DiskFixture::new(false);
    let mut options = fixture.options(fixture.simulator("", 0));
    options.keep = Some(fixture.path("kept"));
    options.out = Some(fixture.path("out/forecast.json"));
    let report = execute(&options, &Fleet::new()).unwrap();
    assert_eq!(
        Record::read(fixture.path("out/forecast.json")).unwrap(),
        report.record
    );
    assert_eq!(
        run_dirs(&report.kept.unwrap()),
        ["target/dev-1", "target/dev-2", "target/holdout-101"]
    );
}

#[test]
fn a_state_directory_without_a_journal_has_no_anchor() {
    let error = Anchor::of_fs(&MemFs::new(), "/empty").unwrap_err();
    assert!(matches!(
        error,
        togi_tool_forecast::Error::ReadJournal { .. }
    ));
    assert_eq!(
        togi_machine::error_chain(&ForecastError::Anchor(Box::new(error))),
        "read anchor: read journal /empty/events.jsonl: entity not found"
    );
}

#[test]
fn a_failed_forecast_read_keeps_the_file_and_original_source() {
    let error = Anchor::of_fs(&MemFs::new(), "/empty").unwrap_err();
    assert_eq!(
        togi_machine::error_chain(&ForecastError::Read(Box::new(error))),
        "read the run: read journal /empty/events.jsonl: entity not found"
    );
}

/// Copies a machine with a facts extract into the fixture and writes a suite that names it by absolute path.
fn absolute_machine_suite(fixture: &DiskFixture) {
    fs::create_dir_all(fixture.path("machines")).unwrap();
    fs::create_dir_all(fixture.path("facts")).unwrap();
    fs::write(
        fixture.path("machines/target-fit-0.json"),
        include_bytes!("../../../tools/bench/machines/target-fit-0.json"),
    )
    .unwrap();
    fs::write(
        fixture.path("facts/target.jsonl.gz"),
        include_bytes!("../../../tools/bench/facts/target.jsonl.gz"),
    )
    .unwrap();
    let absolute = fs::canonicalize(fixture.path("machines/target-fit-0.json")).unwrap();
    fs::write(
        fixture.path("suite.json"),
        format!(
            r#"{{"scenarios":[{{"name":"target","machine":{:?},"dev":[1,2],"holdout":[101]}}]}}"#,
            absolute.to_str().unwrap()
        ),
    )
    .unwrap();
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
    common::assert_snapshot(
        &String::from_utf8(out).unwrap(),
        include_str!("snapshots/forecast__the_forecast_report.snap"),
    );
}

fn command(fixture: &DiskFixture) -> Command {
    command_with_suite(fixture, &fixture.path("suite.json"))
}

/// `forecast` of the fixture's copy with `--suite suite`, run from a fixture repository inside the fixture.
fn command_with_suite(fixture: &DiskFixture, suite: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"));
    isolate_git(&mut command, &fixture.path("githome"));
    command
        .current_dir(repo(fixture.dir.path()))
        .arg("forecast")
        .arg(fixture.state())
        .arg("--suite")
        .arg(suite);
    command
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "real_process coverage exercises CLI path resolution and simulator launch"
)]
fn real_process_a_relative_suite_naming_its_machine_by_absolute_path_names_it_relative_to_the_suite()
 {
    let fixture = DiskFixture::new(false);
    absolute_machine_suite(&fixture);
    let output = command_with_suite(&fixture, Path::new("../suite.json"))
        .arg("--sim")
        .arg(fixture.simulator("", 0))
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record = execute(&fixture.options(fixture.simulator("", 0)), &Fleet::new())
        .unwrap()
        .record;
    assert_eq!(record.runs.len(), 3);
    assert!(
        record
            .runs
            .iter()
            .all(|run| run.machine == "machines/target-fit-0.json"),
        "{:?}",
        record
            .runs
            .iter()
            .map(|run| &run.machine)
            .collect::<Vec<_>>()
    );
    let local = fs::canonicalize(fixture.dir.path()).unwrap();
    assert!(!stdout.contains(local.to_str().unwrap()), "{stdout}");
    assert!(
        !stdout.contains(fixture.dir.path().to_str().unwrap()),
        "{stdout}"
    );
    assert!(
        stdout.contains("\nmachines/target-fit-0.json: "),
        "{stdout}"
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "real_process coverage exercises the command executable and simulator launch"
)]
fn real_process_the_command_prints_the_forecast() {
    let fixture = DiskFixture::new(false);
    let output = command(&fixture)
        .arg("--sim")
        .arg(fixture.simulator("", 0))
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
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "real_process coverage exercises command exit codes for invalid flags"
)]
fn real_process_bench_flags_that_judge_a_run_are_usage_errors_with_forecast() {
    for flag in ["--split=all", "--baseline=b.jsonl", "--same=."] {
        let output = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"))
            .args(["forecast", ".", flag])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{flag}");
        assert!(output.stdout.is_empty(), "{flag}");
    }
}

#[test]
fn a_run_cut_off_by_the_boot_cap_is_a_censored_outcome() {
    let fixture = Fixture::new(false);
    let mut report = fixture.report();
    for run in &mut report.record.runs {
        run.outcome = outcome(&fixture.cut_sessions(run.seed), &fixture.anchor, 3, false).unwrap();
    }
    report.record.summary = togi_tool_forecast::summarize(&report.record.runs).unwrap();
    assert_eq!(report.record.summary.runs, 3);
    assert_eq!(report.record.summary.concluded, 0);
    assert!(
        report
            .record
            .runs
            .iter()
            .all(|run| run.outcome.status == Status::Censored)
    );
}

#[test]
fn an_exit_code_that_disagrees_with_the_journal_names_both() {
    let fixture = Fixture::new(false);
    for (name, cut, exit, message) in [
        (
            "censored exit, concluded journal",
            false,
            3,
            "simulator exit 3 means censored, but the journal after the anchor is concluded",
        ),
        (
            "clean exit, cut journal",
            true,
            0,
            "simulator exit 0 means concluded, but the journal after the anchor is censored",
        ),
    ] {
        let mut sessions = fixture.sessions(SEEDS[0]);
        if cut {
            sessions = fixture.cut_sessions(SEEDS[0]);
        }
        let failed = outcome(&sessions, &fixture.anchor, exit, false).unwrap_err();
        assert!(failed.to_string().contains(message), "{name}: {failed}");
    }
}

#[test]
fn a_suite_that_does_not_parse_fails_as_a_suite_error() {
    let failed = togi_tool_bench::Suite::parse(b"{", PathBuf::from("/suite")).unwrap_err();
    assert!(
        matches!(failed, togi_tool_bench::SuiteError::Decode(_)),
        "{failed}"
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "real_process coverage exercises TMPDIR handling across command children"
)]
fn real_process_a_full_forecast_leaves_no_run_directory_behind() {
    let fixture = DiskFixture::new(false);
    let temporary = fixture.path("tmp");
    fs::create_dir_all(&temporary).unwrap();
    let seen = fixture.path("seen");
    let output = command(&fixture)
        .env("TMPDIR", &temporary)
        .arg("--sim")
        .arg(fixture.simulator(&format!("echo \"$dir\" >> '{}'", seen.display()), 0))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let seen = fs::read_to_string(seen).unwrap();
    assert_eq!(seen.lines().count(), 3);
    assert!(
        seen.lines()
            .all(|dir| Path::new(dir).starts_with(&temporary)),
        "the runs were not made under TMPDIR: {seen}"
    );
    let left: Vec<_> = fs::read_dir(&temporary)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[expect(
    clippy::disallowed_methods,
    reason = "real_process coverage observes failed command cleanup across children"
)]
/// Runs `forecast` under a private `TMPDIR` with a simulator that records its run directory before `extra`, and
/// returns what the failed command left in `TMPDIR` and its stderr. Fails unless runs were made under `TMPDIR`.
fn leftovers_of_a_failed_command(
    fixture: &DiskFixture,
    extra: &str,
    out: &Path,
) -> (Vec<String>, String) {
    let temporary = fixture.path("tmp");
    fs::create_dir_all(&temporary).unwrap();
    let seen = fixture.path("seen");
    let output = command(fixture)
        .env("TMPDIR", &temporary)
        .arg("--sim")
        .arg(fixture.simulator(
            &format!("echo \"$dir\" >> '{}'\n{extra}", seen.display()),
            0,
        ))
        .arg("--out")
        .arg(out)
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!output.status.success(), "{stderr}");
    let seen = fs::read_to_string(seen).unwrap_or_default();
    assert!(
        seen.lines().next().is_some()
            && seen
                .lines()
                .all(|dir| Path::new(dir).starts_with(&temporary)),
        "the runs were not made under TMPDIR: {seen:?}; {stderr}"
    );
    let left = fs::read_dir(&temporary)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    (left, stderr)
}

#[test]
fn real_process_a_forecast_that_fails_to_write_its_record_leaves_no_run_directory_behind() {
    let fixture = DiskFixture::new(false);
    let blocker = fixture.path("file");
    fs::write(&blocker, "").unwrap();
    let (left, stderr) =
        leftovers_of_a_failed_command(&fixture, "", &blocker.join("forecast.json"));
    assert!(stderr.contains("write forecast "), "{stderr}");
    assert!(!stderr.contains("kept the runs"), "{stderr}");
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn real_process_an_interrupted_forecast_leaves_no_run_directory_behind() {
    let fixture = DiskFixture::new(false);
    let (left, stderr) = leftovers_of_a_failed_command(
        &fixture,
        "kill -INT $PPID; sleep 5",
        &fixture.path("forecast.json"),
    );
    assert!(stderr.contains("interrupted"), "{stderr}");
    assert!(!stderr.contains("kept the runs"), "{stderr}");
    assert!(left.is_empty(), "{left:?}");
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "real_process coverage exercises command failure output and retained directories"
)]
fn real_process_the_command_says_where_the_runs_of_a_failed_forecast_are_kept() {
    let fixture = DiskFixture::new(false);
    let temporary = fixture.path("tmp");
    fs::create_dir_all(&temporary).unwrap();
    let output = command(&fixture)
        .env("TMPDIR", &temporary)
        .arg("--sim")
        .arg(fixture.simulator("[ \"$seed\" = 2 ] && exit 2", 0))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let kept = stderr
        .lines()
        .find_map(|line| line.strip_prefix("togi-tool-bench: kept the runs in "))
        .unwrap_or_else(|| panic!("{stderr}"));
    assert!(Path::new(kept).starts_with(&temporary), "{kept}");
    assert_eq!(run_dirs(Path::new(kept)), ["target/dev-2"]);

    let output = command(&fixture)
        .env("TMPDIR", &temporary)
        .arg("--sim")
        .arg(fixture.simulator("", 0))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("kept the runs"));
    fs::remove_dir_all(kept).unwrap();
}

#[test]
fn missing_post_anchor_events_or_a_dead_end_fail_without_a_child_process() {
    let fixture = Fixture::new(false);
    let copied = read_dir_fs(&fixture.history, "/state", Some(fixture.anchor.session)).unwrap();
    assert!(matches!(
        outcome(&copied, &fixture.anchor, 0, false),
        Err(ForecastError::SimulatorFailed { exit: 0 })
    ));
    assert!(matches!(
        outcome(&fixture.sessions(SEEDS[0]), &fixture.anchor, 1, false),
        Err(ForecastError::SimulatorFailed { exit: 1 })
    ));
}
