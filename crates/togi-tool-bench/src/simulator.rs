use std::fs::{self, File};
use std::io::ErrorKind;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use sha2::{Digest, Sha256};
use togi_journal::{FileError, Payload, ReadError, Recording};

use crate::error::{Error, RunError};
use crate::process::{self, Fleet};
use crate::suite::RunSpec;

const JOURNAL: &str = "events.jsonl";
const LOG: &str = "sim.log";
const DEAD_END_LINE: &str = "sim: dead end ";
const BINARY: &str = "togi-tool-sim";

/// The simulator command, run once per session as a subprocess with a state directory of its own.
#[derive(Clone, Debug)]
pub struct Simulator {
    binary: PathBuf,
    digest: String,
}

/// What one simulator process left behind.
#[derive(Debug)]
pub struct Session {
    pub dir: PathBuf,
    pub exit: i32,
    pub wall: Duration,
    pub events: Vec<togi_journal::Event>,
    pub log: String,
}

impl Session {
    pub fn journal_dead_end(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event.payload, Payload::DeadEnd(_)))
    }

    pub fn log_dead_end(&self) -> bool {
        self.log.contains(DEAD_END_LINE)
    }
}

impl Simulator {
    /// The simulator at `explicit`, or the `togi-tool-sim` built from this tree.
    pub fn locate(explicit: Option<&Path>) -> Result<Self, Error> {
        let binary = match explicit {
            Some(path) => path.to_path_buf(),
            None => build()?,
        };
        let bytes = fs::read(&binary).map_err(|source| Error::NoSimulator {
            path: binary.clone(),
            source,
        })?;
        Ok(Self {
            digest: crate::suite::hex(&Sha256::digest(bytes)),
            binary,
        })
    }

    /// The SHA-256 of the simulator's bytes, in lower-case hex: the code the sessions ran.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Runs `spec` in a state directory below `root`, which this call creates, and reads what the process left:
    /// its journal, if it wrote one, and its log. A session that failed or stopped at a dead end or the boot cap
    /// is a result, not an error; a simulator that cannot start or outlives `limit` is an error.
    pub fn run(
        &self,
        fleet: &Fleet,
        root: &Path,
        spec: &RunSpec,
        max_boots: NonZeroU32,
        limit: Duration,
    ) -> Result<Session, RunError> {
        let dir = root
            .join(&spec.scenario)
            .join(format!("{}-{}", spec.split, spec.seed));
        fs::create_dir_all(&dir).map_err(|source| RunError::Directory {
            path: dir.clone(),
            source,
        })?;
        let log_path = dir.join(LOG);
        let log = File::create(&log_path).map_err(|source| RunError::Directory {
            path: log_path.clone(),
            source,
        })?;
        let again = log.try_clone().map_err(|source| RunError::Directory {
            path: log_path.clone(),
            source,
        })?;
        let mut command = Command::new(&self.binary);
        command
            .arg("--seed")
            .arg(spec.seed.to_string())
            .arg("--state-dir")
            .arg(&dir)
            .arg("--max-boots")
            .arg(max_boots.to_string());
        if let Some(machine) = &spec.machine {
            command.arg("--machine").arg(&machine.path);
        }
        if spec.replay.is_some() {
            command.arg("--replay-facts");
        }
        command.stdin(Stdio::null()).stdout(log).stderr(again);
        let finished = process::run(fleet, &mut command, limit)?;
        let events = match Recording::read(&dir.join(JOURNAL)) {
            Ok(recording) => recording.into_events(),
            Err(FileError {
                source: ReadError::Io(error),
                ..
            }) if error.kind() == ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(RunError::Journal(error)),
        };
        let log = fs::read(&log_path).map_err(|source| RunError::Read {
            what: "log",
            source,
        })?;
        Ok(Session {
            dir,
            exit: finished.exit,
            wall: finished.wall,
            events,
            log: String::from_utf8_lossy(&log).into_owned(),
        })
    }
}

/// Builds `togi-tool-sim` from the tree the bench runs in, as the Go bench builds `tools/sim`, and returns the
/// binary cargo reports. Cargo's own progress goes to this process's stderr.
#[expect(
    clippy::disallowed_methods,
    reason = "the bench builds the simulator it benchmarks with cargo"
)]
fn build() -> Result<PathBuf, Error> {
    let failed = |message: String| Error::BuildSimulator { message };
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            BINARY,
            "--message-format=json-render-diagnostics",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| failed(format!("run cargo: {error}")))?;
    if !output.status.success() {
        return Err(failed(format!("cargo build {}", output.status)));
    }
    executable(&output.stdout)
        .ok_or_else(|| failed(format!("cargo reported no {BINARY} executable")))
}

/// The executable of the `togi-tool-sim` binary in cargo's JSON build messages.
fn executable(messages: &[u8]) -> Option<PathBuf> {
    messages
        .split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter(|message| message["target"]["name"] == BINARY)
        .filter(|message| {
            message["target"]["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
        })
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
}
