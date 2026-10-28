use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use super::campaign::{self, Campaign, Halt, Refusal};
use super::ledger::{
    End, Ledger, LedgerError, Outcome, Recipe, Record, Role, StopReason, end_torn_tail,
};
use super::manifest::{Manifest, sha256};
use super::report;
use super::{GitWorkspace, WorkspaceError};

const MANIFEST: &str = "manifest.json";
const LEDGER: &str = "experiments.jsonl";
const LOCK: &str = "lock";
const REPORT: &str = "report.md";
const RAW: &str = "raw";

/// How long an evaluator that was told to stop may take to exit before its process group is killed.
const TERM_GRACE: Duration = Duration::from_secs(5);

/// The run directory's own record, which no evaluator output may name.
pub(super) const RECORD: [&str; 5] = [MANIFEST, LEDGER, LOCK, REPORT, RAW];

#[derive(Debug, thiserror::Error)]
pub enum ResearchError {
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error("{0}; recorded in the ledger")]
    Stopped(Halt),
    #[error("read the checkout")]
    Workspace(#[from] WorkspaceError),
    #[error("read the ledger {}", path.display())]
    Ledger {
        path: PathBuf,
        #[source]
        source: LedgerError,
    },
    #[error("{what} {}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} already exists; a new campaign needs a new run id", .0.display())]
    Exists(PathBuf),
    #[error("another dispatcher holds {}", .0.display())]
    Busy(PathBuf),
    #[error("decode the metrics {}: an object of metric names to numbers", path.display())]
    Metrics {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

fn io_error(what: &'static str, path: &Path) -> impl FnOnce(io::Error) -> ResearchError {
    let path = path.to_owned();
    move |source| ResearchError::Io { what, path, source }
}

/// A campaign's run directory, held exclusively by this dispatcher for as long as the value lives.
#[derive(Debug)]
#[must_use = "dropping the run directory releases its lock"]
pub struct RunDir {
    pub path: PathBuf,
    _lock: File,
}

impl RunDir {
    /// Creates the directory, refusing one that exists, and takes its lock.
    pub fn create(path: &Path) -> Result<Self, ResearchError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_error("create", parent))?;
        }
        match fs::create_dir(path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                return Err(ResearchError::Exists(path.to_owned()));
            }
            Err(source) => return Err(io_error("create", path)(source)),
        }
        let raw = path.join(RAW);
        fs::create_dir(&raw).map_err(io_error("create", &raw))?;
        Self::open(path)
    }

    /// Takes the lock of an existing directory without waiting: one dispatcher acts on a campaign at a time.
    pub fn open(path: &Path) -> Result<Self, ResearchError> {
        let lock_path = path.join(LOCK);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(io_error("open", &lock_path))?;
        match lock.try_lock() {
            Ok(()) => Ok(Self {
                path: path.to_owned(),
                _lock: lock,
            }),
            Err(TryLockError::WouldBlock) => Err(ResearchError::Busy(lock_path)),
            Err(TryLockError::Error(source)) => Err(ResearchError::Io {
                what: "lock",
                path: lock_path,
                source,
            }),
        }
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, ResearchError> {
        let path = self.path.join(name);
        fs::read(&path).map_err(io_error("read", &path))
    }

    pub fn ledger(&self) -> Result<Ledger, ResearchError> {
        Ledger::parse(&self.read(LEDGER)?).map_err(|source| ResearchError::Ledger {
            path: self.path.join(LEDGER),
            source,
        })
    }

    /// Appends records to the ledger, one JSON line each; nothing already written changes. A fragment a failed
    /// append left is first ended with a newline and recorded as `ledger_torn`.
    pub fn append(&self, records: &[Record]) -> Result<(), ResearchError> {
        let path = self.path.join(LEDGER);
        let encode = |source: serde_json::Error| ResearchError::Io {
            what: "encode a record for",
            path: path.clone(),
            source: source.into(),
        };
        let existing = match fs::read(&path) {
            Ok(existing) => existing,
            Err(source) if source.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(source) => return Err(io_error("read", &path)(source)),
        };
        let mut lines = end_torn_tail(&existing, Timestamp::now()).map_err(encode)?;
        for record in records {
            serde_json::to_writer(&mut lines, record).map_err(encode)?;
            lines.push(b'\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(io_error("open", &path))?;
        file.write_all(&lines).map_err(io_error("append to", &path))
    }

    /// Opens the campaign. A missing or replaced `manifest.json` stops a live campaign: nothing may refresh it.
    fn campaign(&self, issue: u32) -> Result<Campaign, ResearchError> {
        let ledger = self.ledger()?;
        let path = self.path.join(MANIFEST);
        let manifest = match fs::read(&path) {
            Ok(manifest) => manifest,
            Err(source)
                if source.kind() == io::ErrorKind::NotFound && ledger.stopped().is_none() =>
            {
                let detail = format!("{MANIFEST} is missing");
                return Err(halted(
                    self,
                    Timestamp::now(),
                    Halt::Stop {
                        reason: StopReason::EvidenceChanged,
                        detail,
                    },
                ));
            }
            Err(source) => return Err(io_error("read", &path)(source)),
        };
        match Campaign::open(&manifest, issue, ledger.clone()) {
            Err(Refusal::ManifestReplaced) if ledger.stopped().is_none() => {
                let detail = format!("{MANIFEST} is not the manifest the campaign started with");
                Err(halted(
                    self,
                    Timestamp::now(),
                    Halt::Stop {
                        reason: StopReason::EvidenceChanged,
                        detail,
                    },
                ))
            }
            result => Ok(result?),
        }
    }
}

/// Appends the stop a halt carries, or passes a refusal through.
fn halted(dir: &RunDir, now: Timestamp, halt: Halt) -> ResearchError {
    match halt {
        Halt::Refuse(refusal) => ResearchError::Refused(refusal),
        Halt::Stop { reason, detail } => {
            let record = Record::CampaignStopped {
                at: now,
                reason,
                detail: detail.clone(),
            };
            match dir.append(&[record]) {
                Ok(()) => ResearchError::Stopped(Halt::Stop { reason, detail }),
                Err(error) => error,
            }
        }
    }
}

/// Validates a manifest against the tree without creating anything.
pub fn check(issue: u32, manifest_path: &Path) -> Result<Manifest, ResearchError> {
    let workspace = GitWorkspace::current()?;
    let bytes = fs::read(manifest_path).map_err(io_error("read", manifest_path))?;
    let (manifest, _) = campaign::start(&bytes, issue, &workspace, Timestamp::now())?;
    Ok(manifest)
}

/// Validates the manifest, creates the run directory with a copy of it and starts the ledger.
pub fn start_run(issue: u32, manifest_path: &Path) -> Result<PathBuf, ResearchError> {
    let workspace = GitWorkspace::current()?;
    let bytes = fs::read(manifest_path).map_err(io_error("read", manifest_path))?;
    let (manifest, record) = campaign::start(&bytes, issue, &workspace, Timestamp::now())?;
    let path = PathBuf::from(&manifest.output);
    let dir = RunDir::create(&path)?;
    let copy = path.join(MANIFEST);
    fs::write(&copy, &bytes).map_err(io_error("write", &copy))?;
    dir.append(&[record])?;
    Ok(path)
}

/// Records runs a killed dispatcher left unfinished as incomplete, and the resumption.
pub fn resume(issue: u32, run: &Path) -> Result<Vec<u32>, ResearchError> {
    let workspace = GitWorkspace::current()?;
    let dir = RunDir::open(run)?;
    let campaign = dir.campaign(issue)?;
    let now = Timestamp::now();
    let records = campaign
        .resume(&workspace, now)
        .map_err(|halt| halted(&dir, now, halt))?;
    dir.append(&records)?;
    Ok(campaign.ledger.dangling())
}

/// Checks and records one evaluator run, runs it under the host-shared lock within the wall time left, keeps
/// its raw output and records how it ended. `waiting` is told once, with the lock's path, when another gate or
/// benchmark holds the host-shared lock; the checks and the time left are read after the wait.
pub fn exec(
    issue: u32,
    run: &Path,
    candidate: &str,
    role: Role,
    recipe: &str,
    args: &[String],
    waiting: &dyn Fn(&Path),
) -> Result<End, ResearchError> {
    let workspace = GitWorkspace::current()?;
    let dir = RunDir::open(run)?;
    let host_lock = workspace.host_lock()?;
    let _host = lock_host(&host_lock, waiting)?;
    let campaign = dir.campaign(issue)?;
    let now = Timestamp::now();
    let planned = campaign
        .plan(&workspace, now, candidate, role, recipe, args)
        .map_err(|halt| halted(&dir, now, halt))?;
    let started = planned.started;
    let stdout_path = dir.path.join(&started.stdout);
    let stderr_path = dir.path.join(&started.stderr);
    let stdout = File::create(&stdout_path).map_err(io_error("create", &stdout_path))?;
    let stderr = File::create(&stderr_path).map_err(io_error("create", &stderr_path))?;
    dir.append(&[Record::ExperimentStarted(started.clone())])?;
    let end = evaluate(
        &workspace.root,
        started.recipe,
        &started.args,
        stdout,
        stderr,
        planned.budget,
    );
    let end = match end {
        Ok(end) => end,
        Err(error) => {
            let records = Campaign::finish(&started, Timestamp::now(), End::Unrunnable, None, None);
            dir.append(&records)?;
            return Err(error);
        }
    };
    let digest = |path: &Path| fs::read(path).ok().map(|bytes| sha256(&bytes));
    let records = Campaign::finish(
        &started,
        Timestamp::now(),
        end,
        digest(&stdout_path),
        digest(&stderr_path),
    );
    dir.append(&records)?;
    Ok(end)
}

/// Takes the lock every gate and benchmark on this host shares, waiting for it; dropping the file releases it.
fn lock_host(path: &Path, waiting: &dyn Fn(&Path)) -> Result<File, ResearchError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(io_error("open", path))?;
    match file.try_lock() {
        Ok(()) => return Ok(file),
        Err(TryLockError::WouldBlock) => waiting(path),
        Err(TryLockError::Error(source)) => {
            return Err(ResearchError::Io {
                what: "lock",
                path: path.to_owned(),
                source,
            });
        }
    }
    file.lock().map_err(io_error("lock", path))?;
    Ok(file)
}

/// What the evaluator's watcher threads report.
enum Event {
    /// The leader has exited and is still unreaped, or watching it failed.
    Exited(io::Result<()>),
    Signal,
}

/// Runs the evaluator's `just` command in its own process group. The group is terminated when the wall
/// time left runs out or the dispatcher is told to stop.
#[expect(
    clippy::disallowed_methods,
    reason = "the dispatcher runs the repository's own evaluator recipes"
)]
fn evaluate(
    root: &Path,
    recipe: Recipe,
    args: &[String],
    stdout: File,
    stderr: File,
    budget: Duration,
) -> Result<End, ResearchError> {
    let (send, receive) = mpsc::channel();
    let mut signals =
        Signals::new([SIGINT, SIGTERM, SIGHUP]).map_err(io_error("watch signals for", root))?;
    let handle = signals.handle();
    let on_signal = send.clone();
    thread::spawn(move || {
        if signals.forever().next().is_some() {
            let _ = on_signal.send(Event::Signal);
        }
    });
    let mut child = Command::new("just")
        .args(recipe.command())
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()
        .map_err(io_error("run just in", root))?;
    let group = Pid::from_child(&child);
    // The leader is only observed here, never reaped: an exited, unreaped leader stays a zombie that holds
    // the group id, so no signal below can reach a recycled id. `evaluate` reaps it after it has decided the
    // end and sent its last signal.
    thread::spawn(move || {
        let observed = waitid(
            WaitId::Pid(group),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        );
        let _ = send.send(Event::Exited(observed.map(drop).map_err(io::Error::from)));
    });
    // The wall limit bounds the evaluator, so stopping it must not wait on the evaluator's goodwill: an
    // evaluator that ignores TERM gets this long to exit, then its group is killed. The campaign may therefore
    // run this much past its wall limit, never longer.
    let terminate = |end: End| -> Result<End, ResearchError> {
        let _ = kill_process_group(group, Signal::TERM);
        let deadline = Instant::now() + TERM_GRACE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match receive.recv_timeout(left) {
                Ok(Event::Exited(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(end),
                Ok(Event::Signal) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        let _ = kill_process_group(group, Signal::KILL);
        loop {
            match receive.recv() {
                Ok(Event::Exited(_)) | Err(_) => return Ok(end),
                Ok(Event::Signal) => {}
            }
        }
    };
    let end = match receive.recv_timeout(budget) {
        Ok(Event::Exited(Ok(()))) => child
            .wait()
            .map(|status| {
                status
                    .code()
                    .map_or(End::Signalled, |code| End::Exited { code })
            })
            .map_err(io_error("wait for just in", root)),
        Ok(Event::Exited(Err(source))) => {
            let _ = child.wait();
            Err(ResearchError::Io {
                what: "wait for just in",
                path: root.to_owned(),
                source,
            })
        }
        Ok(Event::Signal) => {
            let end = terminate(End::Interrupted);
            let _ = child.wait();
            end
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let end = terminate(End::TimedOut);
            let _ = child.wait();
            end
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Ok(End::Unrunnable),
    };
    handle.close();
    end
}

/// Records a candidate's outcome with its metrics, read from a JSON object of names to numbers.
pub fn outcome(
    issue: u32,
    run: &Path,
    candidate: &str,
    outcome: Outcome,
    reason: &str,
    metrics: Option<&Path>,
) -> Result<(), ResearchError> {
    let metrics: BTreeMap<String, f64> = match metrics {
        Some(path) => {
            let bytes = fs::read(path).map_err(io_error("read", path))?;
            serde_json::from_slice(&bytes).map_err(|source| ResearchError::Metrics {
                path: path.to_owned(),
                source,
            })?
        }
        None => BTreeMap::new(),
    };
    let workspace = GitWorkspace::current()?;
    let dir = RunDir::open(run)?;
    let campaign = dir.campaign(issue)?;
    let now = Timestamp::now();
    let records = campaign
        .decide(&workspace, now, candidate, outcome, reason, metrics)
        .map_err(|halt| halted(&dir, now, halt))?;
    dir.append(&records)
}

/// The researcher's stop.
pub fn stop(issue: u32, run: &Path, reason: StopReason, detail: &str) -> Result<(), ResearchError> {
    let dir = RunDir::open(run)?;
    let campaign = dir.campaign(issue)?;
    let record = campaign.stop(Timestamp::now(), reason, detail)?;
    dir.append(&[record])
}

/// Writes `report.md` from the ledger and the researcher's notes on what remains uncertain.
pub fn report(issue: u32, run: &Path, notes: Option<&Path>) -> Result<PathBuf, ResearchError> {
    let dir = RunDir::open(run)?;
    let campaign = dir.campaign(issue)?;
    let notes = match notes {
        Some(path) => fs::read_to_string(path).map_err(io_error("read", path))?,
        None => String::new(),
    };
    let path = dir.path.join(REPORT);
    fs::write(&path, report::render(&campaign, &notes)).map_err(io_error("write", &path))?;
    Ok(path)
}
