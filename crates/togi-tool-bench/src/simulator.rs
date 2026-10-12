use std::fs::{self, File};
use std::io::{ErrorKind, Read as _, Write as _};
use std::num::NonZeroU32;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use togi_journal::{FileError, Payload, ReadError, Recording, ShutdownReason};

use crate::error::{Error, RunError, Unrunnable};
use crate::process::{self, Fleet};
use crate::suite::RunSpec;

const JOURNAL: &str = "events.jsonl";
const LOG: &str = "sim.log";
const DEAD_END_LINE: &str = "sim: dead end ";
const BINARY: &str = "togi-tool-sim";

/// The simulator command, run once per session as a subprocess with a state directory of its own. The bench
/// runs a private copy of the file it hashed, so the digest names the code every session ran even when the
/// original is rebuilt or replaced meanwhile.
#[derive(Clone, Debug)]
pub struct Simulator {
    binary: PathBuf,
    digest: String,
    _private: Arc<tempfile::TempDir>,
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

    /// Whether a signal ended the session rather than the session itself: the process died of one, or its
    /// journal ends in a shutdown a signal asked for. Its outcome is not determined by its inputs.
    pub fn ended_by_signal(&self) -> bool {
        self.exit == -1
            || self.events.iter().any(|event| {
                matches!(&event.payload, Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal)
            })
    }
}

impl Simulator {
    /// The simulator at `explicit`, or the `togi-tool-sim` built from this tree, copied for this bench to run.
    pub fn locate(explicit: Option<&Path>) -> Result<Self, Error> {
        let original = match explicit {
            Some(path) => path.to_path_buf(),
            None => build()?,
        };
        let (bytes, mode) = File::open(&original)
            .and_then(|mut file| {
                let mode = file.metadata()?.permissions().mode();
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Ok((bytes, mode))
            })
            .map_err(|source| Error::NoSimulator {
                path: original.clone(),
                source,
            })?;
        check_runnable(&bytes, mode).map_err(|reason| Error::Unrunnable {
            path: original.clone(),
            reason,
        })?;
        let failed = |source| Error::WriteFile {
            what: "copy the simulator to",
            path: std::env::temp_dir(),
            source,
        };
        let private = tempfile::Builder::new()
            .prefix("togi-bench-sim-")
            .tempdir()
            .map_err(failed)?;
        fs::set_permissions(private.path(), fs::Permissions::from_mode(0o755)).map_err(failed)?;
        let binary = private.path().join(BINARY);
        File::options()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&binary)
            .and_then(|mut file| file.write_all(&bytes))
            .map_err(failed)?;
        let copy = fs::read(&binary).map_err(failed)?;
        Ok(Self {
            digest: crate::suite::hex(&Sha256::digest(copy)),
            binary,
            _private: Arc::new(private),
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

/// Refuses what `exec` would not load. Rust's `Command` runs `execvp` on some platforms and settings, and
/// `execvp` hands a file the kernel rejects with ENOEXEC to `/bin/sh` as a script, so a text file with the
/// execute bit would run as shell code. Checking the bytes the bench is about to copy and run keeps that
/// fallback out of reach on every platform.
fn check_runnable(bytes: &[u8], mode: u32) -> Result<(), Unrunnable> {
    const MAGICS: [&[u8]; 8] = [
        b"\x7fELF",
        b"#!",
        &[0xfe, 0xed, 0xfa, 0xce],
        &[0xfe, 0xed, 0xfa, 0xcf],
        &[0xce, 0xfa, 0xed, 0xfe],
        &[0xcf, 0xfa, 0xed, 0xfe],
        &[0xca, 0xfe, 0xba, 0xbe],
        &[0xbe, 0xba, 0xfe, 0xca],
    ];
    if mode & 0o111 == 0 {
        return Err(Unrunnable::NotExecutable);
    }
    if MAGICS.iter().any(|magic| bytes.starts_with(magic)) {
        Ok(())
    } else {
        Err(Unrunnable::NotAProgram)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_executable_files_in_a_program_format_pass() {
        for bytes in [
            &b"\x7fELF\x02\x01"[..],
            b"#!/bin/sh\n",
            &[0xcf, 0xfa, 0xed, 0xfe, 0x0c],
            &[0xca, 0xfe, 0xba, 0xbe, 0, 0],
        ] {
            assert_eq!(check_runnable(bytes, 0o755), Ok(()), "{bytes:?}");
        }
    }

    #[test]
    fn a_file_the_kernel_would_hand_to_a_shell_is_refused() {
        for bytes in [
            &b"not a program"[..],
            b"echo hi\n",
            b"",
            b"\x7fEL",
            b" #!/bin/sh",
        ] {
            assert_eq!(
                check_runnable(bytes, 0o755),
                Err(Unrunnable::NotAProgram),
                "{bytes:?}"
            );
        }
    }

    #[test]
    fn a_file_without_an_execute_bit_is_refused_whatever_it_holds() {
        assert_eq!(
            check_runnable(b"#!/bin/sh\n", 0o644),
            Err(Unrunnable::NotExecutable)
        );
        assert_eq!(
            check_runnable(b"junk", 0o600),
            Err(Unrunnable::NotExecutable)
        );
    }
}
