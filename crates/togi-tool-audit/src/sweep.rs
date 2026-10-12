//! The suite a sweep hands the bench: every scenario of a bench suite over dev seeds `1..=seeds`, with its holdout
//! and smoke seeds and the suite's gate dropped, since a sweep covers other seeds than the gate scores.

use std::fs;
use std::io::Write;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
pub enum SweepError {
    #[error("decode suite: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("suite has no scenarios")]
    NoScenarios,
    #[error("machine path {0:?} under the suite's directory is not UTF-8")]
    NotUtf8(String),
}

/// Returns the bench suite in `bytes`, whose members the bench itself validates when it runs, as a dev-only sweep over seeds
/// `1..=seeds`, each relative machine path made relative to `base`, the suite's directory, so the sweep suite can
/// live anywhere. `base` must be absolute for the result to be.
pub fn sweep_suite(bytes: &[u8], base: &Path, seeds: NonZeroU64) -> Result<Value, SweepError> {
    let mut suite: Map<String, Value> = serde_json::from_slice(bytes)?;
    suite.remove("gate");
    let scenarios = suite
        .get_mut("scenarios")
        .and_then(Value::as_array_mut)
        .filter(|scenarios| !scenarios.is_empty())
        .ok_or(SweepError::NoScenarios)?;
    let dev: Vec<Value> = (1..=seeds.get()).map(Value::from).collect();
    for scenario in scenarios.iter_mut().filter_map(Value::as_object_mut) {
        if let Some(Value::String(machine)) = scenario.get_mut("machine") {
            resolve(machine, base)?;
        }
        if let Some(Value::Array(machines)) = scenario.get_mut("machines") {
            for machine in machines {
                if let Value::String(machine) = machine {
                    resolve(machine, base)?;
                }
            }
        }
        scenario.remove("holdout");
        scenario.remove("smoke");
        scenario.insert("dev".into(), Value::Array(dev.clone()));
    }
    Ok(Value::Object(suite))
}

fn resolve(machine: &mut String, base: &Path) -> Result<(), SweepError> {
    if machine.is_empty() || Path::new(machine.as_str()).is_absolute() {
        return Ok(());
    }
    let joined = base.join(machine.as_str());
    *machine = joined
        .into_os_string()
        .into_string()
        .map_err(|_| SweepError::NotUtf8(machine.clone()))?;
    Ok(())
}

/// How a sweep runs the bench.
#[derive(Debug)]
pub struct Sweep<'a> {
    pub suite: &'a Path,
    pub seeds: NonZeroU64,
    pub jobs: usize,
    pub timeout_s: u64,
    /// The bench command; `None` means `togi-tool-bench` beside this executable.
    pub bench: Option<&'a Path>,
    /// The simulator command the bench runs; `None` lets the bench build its own.
    pub sim: Option<&'a Path>,
    /// The directory the evidence directory is made in; `None` means a new temporary directory.
    pub keep: Option<&'a Path>,
}

/// What a finished sweep retained.
#[derive(Debug)]
pub struct Evidence {
    pub dir: PathBuf,
    pub records: PathBuf,
    pub root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Suite(#[from] SweepError),
    #[error("{what} {}: {source}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("locate the bench: {0}")]
    Locate(String),
    #[error("bench sweep (evidence {}): {status}", dir.display())]
    Bench { dir: PathBuf, status: String },
    #[error("find the retained bench root in {}: got {found} roots", dir.display())]
    Roots { dir: PathBuf, found: usize },
}

const BENCH: &str = "togi-tool-bench";

fn io(what: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> RunError {
    let path = path.to_path_buf();
    move |source| RunError::Io { what, path, source }
}

impl Sweep<'_> {
    /// Writes the sweep suite and runs the bench over it, keeping every run's state directory. The bench's output
    /// goes to `bench.log` and to this process's stderr.
    pub fn run(&self) -> Result<Evidence, RunError> {
        let bench = match self.bench {
            Some(path) => path.to_path_buf(),
            None => sibling()?,
        };
        let suite_path =
            std::path::absolute(self.suite).map_err(io("resolve suite", self.suite))?;
        let bytes = fs::read(&suite_path).map_err(io("read suite", &suite_path))?;
        let base = suite_path.parent().unwrap_or(Path::new("/"));
        let suite = sweep_suite(&bytes, base, self.seeds)?;
        let parent = match self.keep {
            Some(keep) => {
                fs::create_dir_all(keep).map_err(io("create evidence directory", keep))?;
                keep.to_path_buf()
            }
            None => tempfile::Builder::new()
                .prefix("togi-audit-")
                .tempdir()
                .map_err(io("create evidence directory", Path::new("")))?
                .keep(),
        };
        let dir = tempfile::Builder::new()
            .prefix("sweep-")
            .tempdir_in(&parent)
            .map_err(io("create sweep directory", &parent))?
            .keep();
        let suite_file = dir.join("suite.json");
        let mut encoded = serde_json::to_vec_pretty(&suite).map_err(|error| RunError::Io {
            what: "encode sweep suite",
            path: suite_file.clone(),
            source: error.into(),
        })?;
        encoded.push(b'\n');
        fs::write(&suite_file, encoded).map_err(io("write sweep suite", &suite_file))?;
        let records = dir.join("records.jsonl");
        let log_path = dir.join("bench.log");
        let log = fs::File::create(&log_path).map_err(io("create bench log", &log_path))?;
        let scenarios = suite["scenarios"].as_array().map_or(0, Vec::len);
        let _ = writeln!(
            std::io::stderr(),
            "togi-tool-audit: sweep scenarios={scenarios} seeds={} runs={} evidence={}",
            self.seeds,
            scenarios as u64 * self.seeds.get(),
            dir.display()
        );
        let mut command = Command::new(&bench);
        command
            .arg("--suite")
            .arg(&suite_file)
            .args(["--split", "dev", "--keep"])
            .arg(&dir)
            .arg("--out")
            .arg(&records)
            .arg("--jobs")
            .arg(self.jobs.to_string())
            .arg("--timeout")
            .arg(self.timeout_s.to_string());
        if let Some(sim) = self.sim {
            command.arg("--sim").arg(sim);
        }
        let status = tee(&mut command, log).map_err(io("run the bench", &bench))?;
        if !status.success() {
            return Err(RunError::Bench {
                dir,
                status: status.to_string(),
            });
        }
        let mut roots = Vec::new();
        for entry in fs::read_dir(&dir).map_err(io("find the retained bench root", &dir))? {
            let entry = entry.map_err(io("find the retained bench root", &dir))?;
            if entry.file_name().to_string_lossy().starts_with("bench-") && entry.path().is_dir() {
                roots.push(entry.path());
            }
        }
        let [root] = <[PathBuf; 1]>::try_from(roots).map_err(|roots| RunError::Roots {
            found: roots.len(),
            dir: dir.clone(),
        })?;
        let _ = writeln!(
            std::io::stderr(),
            "togi-tool-audit: records={} root={}",
            records.display(),
            root.display()
        );
        Ok(Evidence { dir, records, root })
    }
}

fn sibling() -> Result<PathBuf, RunError> {
    let exe = std::env::current_exe().map_err(|error| RunError::Locate(error.to_string()))?;
    let path = exe.with_file_name(BENCH);
    if path.is_file() {
        Ok(path)
    } else {
        Err(RunError::Locate(format!(
            "{} is not beside {}; build it with cargo build -p {BENCH} or pass --bench",
            path.display(),
            exe.display()
        )))
    }
}

struct Tee(fs::File);

impl Write for Tee {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(bytes);
        self.0.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Runs `command`, copying its stdout and stderr to this process's stderr and to `log`.
#[expect(
    clippy::disallowed_methods,
    reason = "the audit sweep runs the bench as a subprocess"
)]
fn tee(command: &mut Command, log: fs::File) -> std::io::Result<std::process::ExitStatus> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = child.stdout.take();
    let err = child.stderr.take();
    let second = log.try_clone()?;
    std::thread::scope(|scope| {
        if let Some(mut pipe) = out {
            scope.spawn(move || std::io::copy(&mut pipe, &mut Tee(log)));
        }
        if let Some(mut pipe) = err {
            scope.spawn(move || std::io::copy(&mut pipe, &mut Tee(second)));
        }
    });
    child.wait()
}
