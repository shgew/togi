//! The private OS seam: everything the supervisor does to processes, scopes and `/proc`.
//! The real implementation is Linux-only; unit tests use the scoped fake.

use std::io;
use std::pin::Pin;
use std::process::ExitStatus;

use tokio::io::AsyncRead;
use tokio::time::Instant;

use crate::id::ScopeName;
use crate::identity::BackendIdentity;
use crate::launch::LaunchCommand;
use crate::procfs::{ProcError, ScopeProcess, Threads, Usage};

#[cfg(test)]
pub(crate) mod fake;
#[cfg(target_os = "linux")]
pub(crate) mod linux;

pub(crate) type Pipe = Pin<Box<dyn AsyncRead>>;

pub(crate) struct Pipes {
    pub(crate) stdout: Pipe,
    pub(crate) stderr: Pipe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GroupSignal {
    Cont,
    Term,
    Kill,
    Stop,
}

impl GroupSignal {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Cont => "SIGCONT",
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
            Self::Stop => "SIGSTOP",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SignalError {
    /// The group no longer exists; the unreaped leader keeps its pgid, so this is `ESRCH` only.
    #[error("no such process group")]
    Gone,
    #[error(transparent)]
    Failed(io::Error),
}

/// How the owned launcher ended, read without reaping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExitInfo {
    Code(i32),
    Signal(i32),
}

impl std::fmt::Display for ExitInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(code) => write!(f, "exit status {code}"),
            Self::Signal(signal) => write!(f, "killed by signal {signal}"),
        }
    }
}

impl From<ExitStatus> for ExitInfo {
    fn from(status: ExitStatus) -> Self {
        use std::os::unix::process::ExitStatusExt as _;
        status
            .code()
            .map(Self::Code)
            .or_else(|| status.signal().map(Self::Signal))
            .unwrap_or(Self::Code(-1))
    }
}

/// A launcher the runner owns: the leader stays an unreaped zombie until teardown reaps it, so
/// its pid and process group cannot be reused while anything signals them.
pub(crate) trait Child: 'static {
    fn pid(&self) -> u32;
    fn take_pipes(&mut self) -> io::Result<Pipes>;
    /// Resolves once the leader has exited; safe to drop and call again.
    async fn exited(&self) -> io::Result<()>;
    /// The exit, observed without reaping; `None` while the leader runs.
    fn exit(&self) -> io::Result<Option<ExitInfo>>;
    fn signal_group(&self, signal: GroupSignal) -> Result<(), SignalError>;
    fn reap(self) -> io::Result<ExitInfo>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KillSignal {
    Cont,
    Term,
    Kill,
}

impl KillSignal {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Cont => "SIGCONT",
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }
}

/// A systemd operation on exact trial scopes; no PID or group ever reaches a recovered process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Systemd {
    Kill {
        scope: ScopeName,
        signal: KillSignal,
    },
    Stop {
        scope: ScopeName,
    },
    ListScopes,
    /// The preflight probe: a throwaway scope pinned to CPU 0 that runs `true`.
    Probe {
        unit: String,
        identity: BackendIdentity,
    },
}

/// A launch that failed; `child` is the launcher when it was spawned before the failure, so the
/// rollback still owns, kills and reaps it.
pub(crate) struct LaunchFailure<C> {
    pub(crate) child: Option<C>,
    pub(crate) source: io::Error,
}

impl<C> std::fmt::Debug for LaunchFailure<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchFailure")
            .field("retained", &self.child.is_some())
            .field("source", &self.source)
            .finish()
    }
}

impl<C> From<io::Error> for LaunchFailure<C> {
    fn from(source: io::Error) -> Self {
        Self {
            child: None,
            source,
        }
    }
}

pub(crate) trait Os: 'static {
    type Child: Child;

    fn launch(&self, command: &LaunchCommand) -> Result<Self::Child, LaunchFailure<Self::Child>>;
    fn in_scope(&self, pid: u32, scope: &ScopeName) -> Result<bool, ProcError>;
    fn usage(&self, pid: u32) -> Result<Usage, ProcError>;
    fn threads(&self, pid: u32) -> Result<Threads, ProcError>;
    /// Runs through `togi_command::run`; a timed-out call is an error even when it printed text.
    async fn systemd(
        &self,
        operation: &Systemd,
        deadline: Instant,
    ) -> Result<togi_command::Output, togi_command::Error>;
    /// Every live process in a trial scope. The scan runs off the runtime thread; one that has not
    /// finished by `deadline` is an observation failure.
    async fn scope_processes(&self, deadline: Instant) -> Result<Vec<ScopeProcess>, ProcError>;
    /// How many of `captured` still live, by pid and start time, under the same rule.
    async fn survivors(&self, captured: &[ScopeProcess], deadline: Instant)
    -> Result<usize, ProcError>;
}
