#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Child;
use tokio::time::Instant;

pub const DEFAULT_OUTPUT_LIMIT: usize = 1 << 20;

const REAP_GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    program: OsString,
    args: Vec<OsString>,
    output_limit: usize,
}

impl Invocation {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            output_limit: DEFAULT_OUTPUT_LIMIT,
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    #[must_use]
    pub fn output_limit(mut self, bytes: usize) -> Self {
        self.output_limit = bytes;
        self
    }

    pub fn program(&self) -> &OsStr {
        &self.program
    }

    fn io_error(&self, operation: Operation, source: io::Error) -> Error {
        Error::Io {
            program: self.program.clone(),
            operation,
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl fmt::Display for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Wait,
    Read(Stream),
    StartRuntime,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wait => f.write_str("wait for"),
            Self::Read(stream) => write!(f, "read the {stream} of"),
            Self::StartRuntime => f.write_str("start a runtime for"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot start {}", program.display())]
    Spawn {
        program: OsString,
        #[source]
        source: io::Error,
    },
    #[error("{} did not finish within {timeout:?}", program.display())]
    TimedOut {
        program: OsString,
        timeout: Duration,
    },
    #[error("{} wrote more than {limit} bytes to {stream}", program.display())]
    OutputTooLarge {
        program: OsString,
        stream: Stream,
        limit: usize,
    },
    #[error("cannot {operation} {}", program.display())]
    Io {
        program: OsString,
        operation: Operation,
        #[source]
        source: io::Error,
    },
    #[error("cannot run {} from inside a tokio runtime", program.display())]
    InsideRuntime { program: OsString },
}

/// The child leads its own process group and stays unreaped until the run is decided, so the group id
/// cannot be recycled before the group is killed. Where the exit can only be observed by reaping
/// (not Linux), a child reaped before the deadline is not signalled and only its pipes are dropped.
pub async fn run(invocation: &Invocation, deadline: Instant) -> Result<Output, Error> {
    let timeout = deadline.saturating_duration_since(Instant::now());
    let mut leader = Leader::spawn(invocation)?;
    let stdout = leader.child.stdout.take();
    let stderr = leader.child.stderr.take();

    let finished = tokio::time::timeout_at(deadline, async {
        tokio::try_join!(
            async {
                leader
                    .exited()
                    .await
                    .map_err(|source| invocation.io_error(Operation::Wait, source))
            },
            read_capped(stdout, Stream::Stdout, invocation),
            read_capped(stderr, Stream::Stderr, invocation),
        )
    })
    .await;

    let error = match finished {
        Ok(Ok(((), stdout, stderr))) => {
            let status = leader
                .reap()
                .await
                .map_err(|source| invocation.io_error(Operation::Wait, source))?;
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        Ok(Err(error)) => error,
        Err(_elapsed) => Error::TimedOut {
            program: invocation.program.clone(),
            timeout,
        },
    };
    leader.kill_and_reap().await;
    Err(error)
}

pub fn run_blocking(invocation: &Invocation, timeout: Duration) -> Result<Output, Error> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(Error::InsideRuntime {
            program: invocation.program.clone(),
        });
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| invocation.io_error(Operation::StartRuntime, source))?;
    runtime.block_on(async {
        #[expect(
            clippy::disallowed_methods,
            reason = "run_blocking is run on its own runtime"
        )]
        run(invocation, Instant::now() + timeout).await
    })
}

async fn read_capped(
    pipe: Option<impl AsyncRead + Unpin>,
    stream: Stream,
    invocation: &Invocation,
) -> Result<Vec<u8>, Error> {
    let pipe = pipe.ok_or_else(|| {
        invocation.io_error(Operation::Read(stream), io::ErrorKind::NotConnected.into())
    })?;
    let limit = invocation.output_limit;
    let mut bytes = Vec::new();
    pipe.take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| invocation.io_error(Operation::Read(stream), source))?;
    if bytes.len() > limit {
        return Err(Error::OutputTooLarge {
            program: invocation.program.clone(),
            stream,
            limit,
        });
    }
    Ok(bytes)
}

struct Leader {
    child: Child,
    group: Option<Pid>,
    #[cfg(target_os = "linux")]
    pidfd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
}

impl Leader {
    fn spawn(invocation: &Invocation) -> Result<Self, Error> {
        let mut command = tokio::process::Command::new(&invocation.program);
        command
            .args(&invocation.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        #[expect(
            clippy::disallowed_methods,
            reason = "the one spawn behind togi-command's bounded runner"
        )]
        let spawned = command.spawn();
        let child = spawned.map_err(|source| Error::Spawn {
            program: invocation.program.clone(),
            source,
        })?;
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw);
        #[cfg(target_os = "linux")]
        let pidfd = match group
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
            .and_then(|pid| {
                Ok(rustix::process::pidfd_open(
                    pid,
                    rustix::process::PidfdFlags::empty(),
                )?)
            })
            .and_then(|fd| {
                tokio::io::unix::AsyncFd::with_interest(fd, tokio::io::Interest::READABLE)
            }) {
            Ok(pidfd) => pidfd,
            Err(source) => {
                if let Some(group) = group {
                    kill_group_ignoring_errors(group);
                }
                return Err(invocation.io_error(Operation::Wait, source));
            }
        };
        Ok(Self {
            child,
            group,
            #[cfg(target_os = "linux")]
            pidfd,
        })
    }

    #[cfg(target_os = "linux")]
    async fn exited(&mut self) -> io::Result<()> {
        self.pidfd.readable().await.map(drop)
    }

    #[cfg(not(target_os = "linux"))]
    async fn exited(&mut self) -> io::Result<()> {
        self.child.wait().await?;
        self.group = None;
        Ok(())
    }

    async fn reap(&mut self) -> io::Result<ExitStatus> {
        self.group = None;
        self.child.wait().await
    }

    async fn kill_and_reap(&mut self) {
        self.kill_group();
        let _ = tokio::time::timeout(REAP_GRACE, self.child.wait()).await;
    }

    fn kill_group(&mut self) {
        if let Some(group) = self.group.take() {
            kill_group_ignoring_errors(group);
        }
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        self.kill_group();
    }
}

fn kill_group_ignoring_errors(group: Pid) {
    match kill_process_group(group, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => {}
        Err(_other) => {}
    }
}
