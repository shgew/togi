//! The programs the tool runs: `gh`, `git` in the caller's checkout, and `git` isolated from the caller.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

/// A program that failed, with its subcommand, status and standard error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandError {
    text: String,
    exit_code: Option<i32>,
}

impl CommandError {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            exit_code: None,
        }
    }

    /// A failure `text` that exited with `code`, worded `text: exit status code`.
    pub fn exit(text: &str, code: i32) -> Self {
        Self {
            text: format!("{text}: exit status {code}"),
            exit_code: Some(code),
        }
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for CommandError {}

pub type Output = Result<Vec<u8>, CommandError>;

pub trait Programs {
    /// Runs `gh` with `args` and returns its standard output.
    fn gh(&mut self, args: &[&str]) -> Output;
    /// Runs `git` with `args` in the caller's checkout.
    fn git(&mut self, args: &[&str]) -> Output;
    /// Runs `git` with `args` in `dir`, with no configuration, attributes or Git environment from the caller,
    /// so the same arguments give the same result on any host.
    fn isolated_git(&mut self, dir: &Path, args: &[&str]) -> Output;
    /// Runs the isolated `git` as `isolated_git` does, with `input` as its standard input.
    fn isolated_git_input(&mut self, dir: &Path, args: &[&str], input: &[u8]) -> Output;
}

/// The real programs, run with `env` as their whole environment and in `dir` when set.
pub struct System {
    pub dir: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
}

impl System {
    pub fn from_process() -> Self {
        Self {
            dir: None,
            env: std::env::vars_os().collect(),
        }
    }

    fn command(&self, program: &str, args: &[&str], isolated: bool) -> Command {
        let mut command = Command::new(program);
        command.env_clear().stdin(Stdio::null());
        if let Some(dir) = &self.dir {
            command.current_dir(dir);
        }
        if isolated {
            command
                .args(["-c", "core.attributesFile=/dev/null"])
                .envs(
                    self.env
                        .iter()
                        .filter(|(key, _)| !key.as_encoded_bytes().starts_with(b"GIT_"))
                        .map(|(key, value)| (key, value)),
                )
                .envs([
                    ("GIT_CONFIG_NOSYSTEM", "1"),
                    ("GIT_CONFIG_GLOBAL", "/dev/null"),
                    ("GIT_ATTR_NOSYSTEM", "1"),
                ]);
        } else {
            command.envs(self.env.iter().map(|(key, value)| (key, value)));
        }
        command.args(args);
        command
    }
}

impl Programs for System {
    fn gh(&mut self, args: &[&str]) -> Output {
        run(self.command("gh", args, false), "gh", args, None)
    }

    fn git(&mut self, args: &[&str]) -> Output {
        run(self.command("git", args, false), "git", args, None)
    }

    fn isolated_git(&mut self, dir: &Path, args: &[&str]) -> Output {
        let mut command = self.command("git", args, true);
        command.current_dir(dir);
        run(command, "git", args, None)
    }

    fn isolated_git_input(&mut self, dir: &Path, args: &[&str], input: &[u8]) -> Output {
        let mut command = self.command("git", args, true);
        command.current_dir(dir);
        run(command, "git", args, Some(input))
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the review tool's whole job is running gh and git and reading their output"
)]
fn run(mut command: Command, program: &str, args: &[&str], input: Option<&[u8]>) -> Output {
    let sub = args.first().copied().unwrap_or_default();
    let failed = |error: &io::Error| {
        let reason = if error.kind() == io::ErrorKind::NotFound {
            format!(
                "exec: {}: executable file not found in $PATH",
                crate::text::quote(program)
            )
        } else {
            crate::error::io_reason(error)
        };
        CommandError::new(format!("{program} {sub}: {reason}: "))
    };
    let output = match input {
        None => command.output().map_err(|error| failed(&error))?,
        Some(input) => {
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| failed(&error))?;
            let stdin = child.stdin.take();
            // The input is written while the output is read, so neither side waits on a full pipe. Dropping
            // the handle at the end of the write closes the program's standard input.
            let (output, written) = std::thread::scope(|scope| {
                let writer =
                    scope.spawn(move || stdin.map_or(Ok(()), |mut stdin| stdin.write_all(input)));
                let output = child.wait_with_output();
                let written = writer
                    .join()
                    .unwrap_or_else(|_| Err(io::Error::other("the input writer panicked")));
                (output, written)
            });
            let output = output.map_err(|error| failed(&error))?;
            // A program that fails may stop reading early; its own status and message then explain more
            // than the broken pipe does, so the write error counts only when the program succeeded.
            if output.status.success() {
                written.map_err(|error| failed(&error))?;
            }
            output
        }
    };
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(CommandError {
        text: format!(
            "{program} {sub}: {}: {}",
            status_text(output.status),
            stderr.trim()
        ),
        exit_code: output.status.code(),
    })
}

fn status_text(status: ExitStatus) -> String {
    status
        .code()
        .map_or_else(|| status.to_string(), |code| format!("exit status {code}"))
}
