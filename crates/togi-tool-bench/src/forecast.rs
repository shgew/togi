//! `forecast`: what the simulator expects a real run to do, from a copy of its state directory.
//!
//! The ensemble is every dev and holdout run of the suite's [`SCENARIO`] scenario. Each run resumes its own copy of
//! the state directory on the simulator until the session concludes, as `togi run --cycles 1` would, and what it
//! did after the [`Anchor`], the last event of the copied live journal, becomes its [`Outcome`]. The runs, their
//! summary and the files they read make the [`Record`], which `stats` later scores the real run against. The
//! given directory is only read.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use togi_journal::{Event, Payload};
use togi_tool_forecast::{Anchor, File, Outcome, Record, Run, Session, Status, after, read_dir};
use togi_tool_modelcheck::Check;
use togi_tool_trialfacts::Extracts;

use crate::error::{Error, RunError};
use crate::git::Tree;
use crate::pool;
use crate::process::Fleet;
use crate::record::Status as RunStatus;
use crate::run::check_machine;
use crate::simulator::Simulator;
use crate::suite::{Machine, RunSpec, Selection, Suite};

/// The suite scenario whose runs form the forecast's ensemble.
pub const SCENARIO: &str = "target";

const LIVE: &str = "events.jsonl";
const STATE: &str = "state.json";
const ARCHIVE: &str = "archive";
const SESSION_BEARING: [&str; 4] = [".jsonl", "-carry-pending", "-compat-pending", "-reset-all"];

/// What one forecast does.
#[derive(Clone, Debug)]
pub struct Options {
    /// The copy of the real run's state directory.
    pub state: PathBuf,
    pub suite: PathBuf,
    /// The tree the forecast runs from, stamped on the record.
    pub tree: Tree,
    /// Writes the record here.
    pub out: Option<PathBuf>,
    /// Keeps every run's directory under a new directory in this one.
    pub keep: Option<PathBuf>,
    pub jobs: NonZeroUsize,
    pub timeout: Duration,
    pub max_boots: NonZeroU32,
    /// A simulator to run instead of building `togi-tool-sim` from this tree.
    pub simulator: Option<PathBuf>,
}

/// Why a forecast could not be made.
#[derive(Debug, thiserror::Error)]
pub enum ForecastError {
    #[error("read anchor: {0}")]
    Anchor(#[source] Box<togi_tool_forecast::Error>),
    #[error(transparent)]
    Bench(Box<Error>),
    #[error("suite {} has no {SCENARIO} scenario", suite.display())]
    NoEnsemble { suite: PathBuf },
    #[error("ensemble file {} is absent or outside the suite's directory", path.display())]
    EnsembleFile { path: PathBuf },
    #[error("{what} {}: {source}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("archive entry {} is not a regular file", path.display())]
    IrregularArchiveEntry { path: PathBuf },
    #[error("start the simulator: {0}")]
    Launch(#[source] RunError),
    #[error("read the run: {0}")]
    Read(#[source] Box<togi_tool_forecast::Error>),
    #[error("simulator exit {exit} means {means}, but the journal after the anchor is {journal}")]
    StatusMismatch {
        exit: i32,
        means: &'static str,
        journal: &'static str,
    },
    #[error("simulator failed with exit {exit}")]
    SimulatorFailed { exit: i32 },
    #[error("{0}")]
    Runs(RunFailures),
    #[error("summarize the forecast: {0}")]
    Summary(#[source] Box<togi_tool_forecast::Error>),
    #[error("write forecast {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("interrupted")]
    Interrupted,
}

impl From<Error> for ForecastError {
    fn from(error: Error) -> Self {
        Self::Bench(Box::new(error))
    }
}

/// The runs of a forecast that failed, by run, in suite order.
#[derive(Debug)]
pub struct RunFailures(pub Vec<(String, ForecastError)>);

impl fmt::Display for RunFailures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (n, (run, error)) in self.0.iter().enumerate() {
            if n > 0 {
                writeln!(f)?;
            }
            write!(f, "forecast {run}: {error}")?;
        }
        Ok(())
    }
}

/// A forecast that failed, and the run directory it left for inspection, if any.
#[derive(Debug)]
pub struct Failed {
    pub error: Box<ForecastError>,
    pub kept: Option<PathBuf>,
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for Failed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// A finished forecast: the record and the model checks of the machines it ran.
#[derive(Debug)]
pub struct Report {
    pub record: Record,
    pub checks: Vec<Check>,
    /// The run directory kept.
    pub kept: Option<PathBuf>,
}

impl Report {
    /// Writes the forecast: its anchor, the runs by status, each metric's range and the model checks. The output
    /// holds no wall time and no local path.
    pub fn write(&self, w: &mut impl io::Write) -> io::Result<()> {
        let record = &self.record;
        let summary = &record.summary;
        let commit = if record.dirty {
            format!("{} (dirty)", record.commit)
        } else {
            record.commit.clone()
        };
        writeln!(
            w,
            "forecast: after session {} event {} at {}",
            record.anchor.session, record.anchor.seq, record.anchor.time
        )?;
        writeln!(
            w,
            "forecast: commit {commit}, ruleset {}, {} {SCENARIO} runs: {} concluded, {} dead ends, {} censored",
            record.ruleset, summary.runs, summary.concluded, summary.dead_ends, summary.censored
        )?;
        writeln!(
            w,
            "forecast: hours run from the anchor to conclusion, over concluded runs only; {}",
            togi_tool_forecast::recovery_bias()
        )?;
        let metrics = togi_tool_forecast::metrics(&record.runs)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut rows = vec![[
            "metric".to_owned(),
            "median".to_owned(),
            "p10..p90".to_owned(),
            "min..max".to_owned(),
        ]];
        for metric in &metrics {
            rows.push(match &metric.range {
                None => [metric.kind.to_string(), "-".into(), "-".into(), "-".into()],
                Some(range) => [
                    metric.kind.to_string(),
                    metric.format(range.median),
                    format!("{}..{}", metric.format(range.p10), metric.format(range.p90)),
                    format!("{}..{}", metric.format(range.min), metric.format(range.max)),
                ],
            });
        }
        write_table(w, &rows)?;
        togi_tool_modelcheck::report(w, &self.checks)
    }
}

/// Writes `rows` as columns left-aligned two spaces apart, the last column unpadded.
fn write_table(w: &mut impl io::Write, rows: &[[String; 4]]) -> io::Result<()> {
    let widths: [usize; 3] = std::array::from_fn(|column| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or(0)
            + 2
    });
    for row in rows {
        writeln!(
            w,
            "{:<w0$}{:<w1$}{:<w2$}{}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
        )?;
    }
    Ok(())
}

/// Forecasts the real run whose state directory is copied at `options.state`.
pub fn execute(options: &Options, fleet: &Fleet) -> Result<Report, Failed> {
    let fail = |error: ForecastError| Failed {
        error: Box::new(error),
        kept: None,
    };
    let anchor = Anchor::of(&options.state)
        .map_err(|error| ForecastError::Anchor(Box::new(error)))
        .map_err(fail)?;
    let runs = ensemble(&options.suite).map_err(fail)?;
    let checks = model_checks(&runs).map_err(fail)?;
    let files = ensemble_files(&runs).map_err(fail)?;
    let machines = runs
        .iter()
        .map(|spec| spec.machine.as_deref().map(machine_name).transpose())
        .collect::<Result<Vec<_>, _>>()
        .map_err(fail)?;
    let simulator = Simulator::locate(options.simulator.as_deref())
        .map_err(ForecastError::from)
        .map_err(fail)?;
    let root = run_root(options.keep.as_deref()).map_err(fail)?;
    let result = forecast_runs(options, fleet, &simulator, &root, &anchor, &runs);
    let keep = options.keep.is_some() || matches!(result, Err(ForecastError::Runs(_)));
    let finish = |error: ForecastError| {
        let kept = if keep {
            Some(root.clone())
        } else {
            let _ = fs::remove_dir_all(&root);
            None
        };
        Failed {
            error: Box::new(error),
            kept,
        }
    };
    let outcomes = result.map_err(finish)?;
    let ran = runs
        .iter()
        .zip(machines)
        .zip(outcomes)
        .map(|((spec, machine), outcome)| Run {
            machine: machine.unwrap_or_default(),
            seed: spec.seed,
            split: spec.split.to_string(),
            outcome,
        })
        .collect();
    let record = Record::new(
        anchor,
        options.tree.commit.clone(),
        options.tree.dirty,
        files,
        ran,
    )
    .map_err(|error| ForecastError::Summary(Box::new(error)))
    .map_err(finish)?;
    if let Some(out) = &options.out {
        write_record(out, &record).map_err(finish)?;
    }
    let kept = if keep {
        Some(root)
    } else {
        let _ = fs::remove_dir_all(&root);
        None
    };
    Ok(Report {
        record,
        checks,
        kept,
    })
}

/// Every dev and holdout run of the suite's forecast scenario.
fn ensemble(suite: &Path) -> Result<Vec<RunSpec>, ForecastError> {
    let plan = Suite::load(suite)
        .map_err(Error::from)
        .and_then(|suite| suite.plan(Selection::All))?;
    let runs: Vec<RunSpec> = plan
        .runs
        .into_iter()
        .filter(|spec| spec.scenario == SCENARIO)
        .collect();
    if runs.is_empty() {
        return Err(ForecastError::NoEnsemble {
            suite: suite.to_path_buf(),
        });
    }
    Ok(runs)
}

/// The model check of each distinct machine the ensemble runs that names a facts extract, naming the machine and
/// its extract as the record does.
fn model_checks(runs: &[RunSpec]) -> Result<Vec<Check>, ForecastError> {
    let mut extracts = Extracts::new();
    let mut seen = BTreeSet::new();
    let mut checks = Vec::new();
    for machine in runs.iter().filter_map(|spec| spec.machine.as_ref()) {
        if seen.insert(&machine.path)
            && let Some(mut check) = check_machine(machine, &mut extracts)?
        {
            let name = PathBuf::from(machine_name(machine)?);
            if let Some(facts) = machine.definition.facts.as_deref() {
                check.extract = name.parent().unwrap_or_else(|| Path::new("")).join(facts);
            }
            check.machine = name;
            checks.push(check);
        }
    }
    Ok(checks)
}

/// The machine file by its path relative to the suite's directory, the first of its inputs: the suite may name it
/// by an absolute path, which must reach neither the record nor the report.
fn machine_name(machine: &Machine) -> Result<String, ForecastError> {
    let path = machine
        .inputs
        .first()
        .map_or(machine.path.as_path(), |(path, _)| path.as_path());
    match (path.is_absolute(), path.to_str()) {
        (false, Some(name)) => Ok(name.replace('\\', "/")),
        _ => Err(ForecastError::EnsembleFile {
            path: path.to_path_buf(),
        }),
    }
}

/// Each machine file the runs load, parents included, and each facts extract they read, in first-use order, by
/// path relative to the suite's directory with its SHA-256.
fn ensemble_files(runs: &[RunSpec]) -> Result<Vec<File>, ForecastError> {
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    for machine in runs.iter().filter_map(|spec| spec.machine.as_ref()) {
        for (path, sum) in &machine.inputs {
            if !seen.insert(path) {
                continue;
            }
            let (Some(sum), false, Some(name)) = (sum, path.is_absolute(), path.to_str()) else {
                return Err(ForecastError::EnsembleFile { path: path.clone() });
            };
            files.push(File {
                path: name.replace('\\', "/"),
                sha256: sum.clone(),
            });
        }
    }
    Ok(files)
}

fn run_root(keep: Option<&Path>) -> Result<PathBuf, ForecastError> {
    let failed = |source| ForecastError::Io {
        what: "create the run directory",
        path: keep.map(Path::to_path_buf).unwrap_or_default(),
        source,
    };
    match keep {
        Some(keep) => {
            fs::create_dir_all(keep).map_err(failed)?;
            tempfile::Builder::new()
                .prefix("forecast-")
                .tempdir_in(keep)
                .map(tempfile::TempDir::keep)
                .map_err(failed)
        }
        None => tempfile::Builder::new()
            .prefix("togi-forecast-runs-")
            .tempdir()
            .map(tempfile::TempDir::keep)
            .map_err(failed),
    }
}

/// Runs the ensemble from copies of the state directory and summarizes what each run did after the anchor, in
/// suite order whatever the order they finish in. Once `fleet` is cancelled no more runs start and the forecast
/// fails as interrupted rather than with failed runs.
fn forecast_runs(
    options: &Options,
    fleet: &Fleet,
    simulator: &Simulator,
    root: &Path,
    anchor: &Anchor,
    runs: &[RunSpec],
) -> Result<Vec<Outcome>, ForecastError> {
    let outcomes: Mutex<Vec<Option<Outcome>>> = Mutex::new((0..runs.len()).map(|_| None).collect());
    let order: Vec<usize> = (0..runs.len()).collect();
    let mut failures = pool::run(options.jobs.get(), &order, |i| {
        let outcome = forecast_run(options, fleet, simulator, root, anchor, &runs[i])
            .inspect_err(|_| fleet.abort())?;
        outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[i] = Some(outcome);
        Ok(())
    });
    if fleet.cancelled() {
        return Err(ForecastError::Interrupted);
    }
    failures.retain(|(_, error)| !matches!(error, ForecastError::Launch(RunError::Interrupted)));
    if !failures.is_empty() {
        failures.sort_by_key(|(i, _)| *i);
        return Err(ForecastError::Runs(RunFailures(
            failures
                .into_iter()
                .map(|(i, error)| (runs[i].name(), error))
                .collect(),
        )));
    }
    Ok(outcomes
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .flatten()
        .collect())
}

/// Resumes one copy of the state directory on the simulator. An error fails the forecast.
fn forecast_run(
    options: &Options,
    fleet: &Fleet,
    simulator: &Simulator,
    root: &Path,
    anchor: &Anchor,
    spec: &RunSpec,
) -> Result<Outcome, ForecastError> {
    let dir = root
        .join(&spec.scenario)
        .join(format!("{}-{}", spec.split, spec.seed));
    copy_state(&options.state, &dir)?;
    let launched = simulator
        .launch(fleet, root, spec, options.max_boots, options.timeout)
        .map_err(|error| {
            if matches!(error, RunError::Interrupted) && options.keep.is_none() {
                let _ = fs::remove_dir_all(&dir);
            }
            ForecastError::Launch(error)
        })?;
    let sessions = read_dir(&dir, Some(anchor.session))
        .map_err(|error| ForecastError::Read(Box::new(error)))?;
    let mut outcome =
        after(&sessions, anchor).map_err(|error| ForecastError::Read(Box::new(error)))?;
    let events = events_after(&sessions, anchor);
    let dead_end = events
        .iter()
        .any(|event| matches!(event.payload, Payload::DeadEnd(_)));
    let status = RunStatus::of(
        launched.exit,
        !events.is_empty(),
        dead_end,
        launched.log_dead_end(),
    );
    // Exit 1 is a dead end only when this invocation reached one: a dead end event after the anchor, or its own
    // log line when it finished one the copy left pending. A dead end in the copy is not this run's.
    let (means, journal, expected) = match status {
        RunStatus::DeadEnd => {
            outcome.status = Status::DeadEnd;
            ("dead end", "", Status::DeadEnd)
        }
        RunStatus::Concluded => ("concluded", name(outcome.status), Status::Concluded),
        RunStatus::Censored => ("censored", name(outcome.status), Status::Censored),
        RunStatus::Error => {
            return Err(ForecastError::SimulatorFailed {
                exit: launched.exit,
            });
        }
    };
    if outcome.status != expected {
        return Err(ForecastError::StatusMismatch {
            exit: launched.exit,
            means,
            journal,
        });
    }
    if options.keep.is_none() {
        fs::remove_dir_all(&dir).map_err(|source| ForecastError::Io {
            what: "remove the run",
            path: dir,
            source,
        })?;
    }
    Ok(outcome)
}

const fn name(status: Status) -> &'static str {
    match status {
        Status::Concluded => "concluded",
        Status::DeadEnd => "a dead end",
        Status::Censored => "censored",
    }
}

/// The events the sessions record after the anchor, oldest first.
fn events_after<'a>(sessions: &'a [Session], anchor: &Anchor) -> Vec<&'a Event> {
    let mut after = Vec::new();
    let mut reached = false;
    for session in sessions {
        if session.id() == anchor.session {
            reached = true;
            after.extend(
                session
                    .events()
                    .iter()
                    .filter(|event| event.seq > anchor.seq),
            );
        } else if reached {
            after.extend(session.events());
        }
    }
    after
}

/// Copies the files a resumed session reads from `src` into `dst`: the live journal, `state.json` and every
/// session-bearing archive entry (archived journals, the pending carry and compatibility markers, and `reset
/// --all` markers). `src` is only read.
fn copy_state(src: &Path, dst: &Path) -> Result<(), ForecastError> {
    let io = |what, path: &Path| {
        let path = path.to_path_buf();
        move |source| ForecastError::Io { what, path, source }
    };
    fs::create_dir_all(dst).map_err(io("create the run", dst))?;
    let mut files = vec![PathBuf::from(LIVE)];
    match fs::metadata(src.join(STATE)) {
        Ok(_) => files.push(PathBuf::from(STATE)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ForecastError::Io {
                what: "read the state copy",
                path: src.join(STATE),
                source,
            });
        }
    }
    let archive = src.join(ARCHIVE);
    let mut archived = Vec::new();
    match fs::read_dir(&archive) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(io("read the archive copy", &archive))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if !SESSION_BEARING.iter().any(|suffix| name.ends_with(suffix)) {
                    continue;
                }
                let regular = entry.file_type().is_ok_and(|kind| kind.is_file());
                if !regular {
                    return Err(ForecastError::IrregularArchiveEntry { path: entry.path() });
                }
                archived.push(Path::new(ARCHIVE).join(name));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ForecastError::Io {
                what: "read the archive copy",
                path: archive,
                source,
            });
        }
    }
    archived.sort();
    if !archived.is_empty() {
        fs::create_dir_all(dst.join(ARCHIVE)).map_err(io("create the run archive", dst))?;
    }
    for name in files.into_iter().chain(archived) {
        fs::copy(src.join(&name), dst.join(&name))
            .map_err(io("copy the state", &src.join(&name)))?;
    }
    Ok(())
}

fn write_record(path: &Path, record: &Record) -> Result<(), ForecastError> {
    let failed = |source: Box<dyn std::error::Error + Send + Sync>| ForecastError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| failed(error.into()))?;
    }
    let mut file = fs::File::create(path).map_err(|error| failed(error.into()))?;
    record
        .write(&mut file)
        .map_err(|error| failed(error.into()))?;
    file.sync_all().map_err(|error| failed(error.into()))
}
