//! The git seam: every repository read and write the release makes is one git command.

use std::io::{self, Write as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::string::FromUtf8Error;
use std::{fs, thread};

/// One git invocation: its arguments, extra environment variables and standard input.
#[derive(Clone, Copy, Debug)]
pub struct GitCommand<'a> {
    pub args: &'a [&'a str],
    pub env: &'a [(&'a str, &'a str)],
    pub stdin: &'a str,
}

impl<'a> GitCommand<'a> {
    /// `git args` with no extra environment and empty input.
    pub fn new(args: &'a [&'a str]) -> Self {
        Self {
            args,
            env: &[],
            stdin: "",
        }
    }
}

/// Why a git command failed.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("start git {command}")]
    Start {
        command: String,
        #[source]
        source: io::Error,
    },
    /// `status` reads as Go's `exec.ExitError` does, as in `exit status 128` or `signal: killed`.
    #[error("git {command}: {}: {stderr}", go_status(*status))]
    Failed {
        command: String,
        status: ExitStatus,
        stderr: String,
    },
    #[error("read the output of git {command}")]
    Output {
        command: String,
        #[source]
        source: FromUtf8Error,
    },
}

/// The repository the release reads and writes.
pub trait Git {
    /// Runs `command` and returns its standard output.
    fn run(&mut self, command: GitCommand<'_>) -> Result<String, GitError>;

    /// Deletes the scratch file at `path`, a path git printed relative to the repository, if it exists.
    fn remove_scratch(&mut self, path: &str);
}

/// Runs the `git` executable in a directory.
#[derive(Clone, Debug)]
pub struct ProcessGit {
    dir: PathBuf,
    env: Vec<(String, String)>,
    removed_env: Vec<String>,
}

impl ProcessGit {
    /// Runs git in `dir` with the process's environment.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            env: Vec::new(),
            removed_env: Vec::new(),
        }
    }

    /// Also sets `key` to `value` for every command.
    #[must_use]
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    /// Also removes `key` from every command's environment.
    #[must_use]
    pub fn env_remove(mut self, key: &str) -> Self {
        self.removed_env.push(key.to_owned());
        self
    }
}

impl Git for ProcessGit {
    #[expect(
        clippy::disallowed_methods,
        reason = "the release tool's one call site that runs git"
    )]
    fn run(&mut self, command: GitCommand<'_>) -> Result<String, GitError> {
        let rendered = || command.args.join(" ");
        let mut process = Command::new("git");
        process.current_dir(&self.dir).args(command.args);
        for key in &self.removed_env {
            process.env_remove(key);
        }
        process
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .envs(command.env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let start = |source| GitError::Start {
            command: rendered(),
            source,
        };
        let mut child = process.spawn().map_err(start)?;
        let stdin = child.stdin.take();
        let output = thread::scope(|scope| {
            let writer = scope.spawn(move || match stdin {
                Some(mut stdin) => stdin.write_all(command.stdin.as_bytes()),
                None => Ok(()),
            });
            let output = child.wait_with_output();
            let written = writer
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the stdin writer panicked")));
            output.and_then(|output| {
                // git may exit before reading all of its input; its status then tells what happened.
                match written {
                    Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error),
                    Ok(()) | Err(_) => Ok(output),
                }
            })
        })
        .map_err(start)?;
        if !output.status.success() {
            return Err(GitError::Failed {
                command: rendered(),
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        String::from_utf8(output.stdout).map_err(|source| GitError::Output {
            command: rendered(),
            source,
        })
    }

    fn remove_scratch(&mut self, path: &str) {
        // A leftover file is harmless: the next release's `read-tree` replaces the whole index.
        let _ = fs::remove_file(self.dir.join(path));
    }
}

/// The names Go gives signals 1 to 31 on this platform.
#[cfg(target_os = "macos")]
const SIGNALS: [&str; 31] = [
    "hangup",
    "interrupt",
    "quit",
    "illegal instruction",
    "trace/BPT trap",
    "abort trap",
    "EMT trap",
    "floating point exception",
    "killed",
    "bus error",
    "segmentation fault",
    "bad system call",
    "broken pipe",
    "alarm clock",
    "terminated",
    "urgent I/O condition",
    "suspended (signal)",
    "suspended",
    "continued",
    "child exited",
    "stopped (tty input)",
    "stopped (tty output)",
    "I/O possible",
    "cputime limit exceeded",
    "filesize limit exceeded",
    "virtual timer expired",
    "profiling timer expired",
    "window size changes",
    "information request",
    "user defined signal 1",
    "user defined signal 2",
];

/// The names Go gives signals 1 to 31 on this platform.
#[cfg(not(target_os = "macos"))]
const SIGNALS: [&str; 31] = [
    "hangup",
    "interrupt",
    "quit",
    "illegal instruction",
    "trace/breakpoint trap",
    "aborted",
    "bus error",
    "floating point exception",
    "killed",
    "user defined signal 1",
    "segmentation fault",
    "user defined signal 2",
    "broken pipe",
    "alarm clock",
    "terminated",
    "stack fault",
    "child exited",
    "continued",
    "stopped (signal)",
    "stopped",
    "stopped (tty input)",
    "stopped (tty output)",
    "urgent I/O condition",
    "CPU time limit exceeded",
    "file size limit exceeded",
    "virtual timer expired",
    "profiling timer expired",
    "window changed",
    "I/O possible",
    "power failure",
    "bad system call",
];

/// `status` as Go's `os.ProcessState` prints a finished process: `exit status N`, or `signal: NAME` for a
/// killed one, followed by ` (core dumped)` if it dumped core.
fn go_status(status: ExitStatus) -> String {
    let mut text = match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit status {code}"),
        (None, Some(signal)) => {
            let name = usize::try_from(signal)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| SIGNALS.get(i));
            match name {
                Some(name) => format!("signal: {name}"),
                None => format!("signal: signal {signal}"),
            }
        }
        (None, None) => status.to_string(),
    };
    if status.core_dumped() {
        text.push_str(" (core dumped)");
    }
    text
}

#[cfg(test)]
#[path = "tests/git.rs"]
mod tests;
