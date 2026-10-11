#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use rustix::io::Errno;
use rustix::process::{Pid, Signal, WaitOptions, kill_process_group, waitpid};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::time::Instant;

pub const DEFAULT_OUTPUT_LIMIT: usize = 1 << 20;

/// How long teardown waits for the killed leader before leaving it to a reaper thread.
const REAP_GRACE: Duration = Duration::from_secs(1);
const REAP_POLL: Duration = Duration::from_millis(5);
#[cfg(not(target_os = "linux"))]
const EXIT_POLL: Duration = Duration::from_millis(10);

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
    StartReaper,
    KillGroup,
    Reap,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wait => f.write_str("wait for"),
            Self::Read(stream) => write!(f, "read the {stream} of"),
            Self::StartRuntime => f.write_str("start a runtime for"),
            Self::StartReaper => f.write_str("start a reaper thread for"),
            Self::KillGroup => f.write_str("kill the process group of"),
            Self::Reap => f.write_str("reap"),
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
    /// Tearing the command down after `cause` failed: the process group could not be signalled
    /// (`KillGroup`) and may still be running, or the leader was not reaped within the grace
    /// (`Reap`) and is left to a reaper thread.
    #[error("{cause}; then cannot {operation} {}", program.display())]
    Teardown {
        program: OsString,
        cause: Box<Error>,
        operation: Operation,
        #[source]
        source: io::Error,
    },
}

/// The child leads its own process group and stays unreaped until the run is decided, so the group id
/// cannot be recycled before the group is killed. Where the exit can only be observed by reaping
/// (not Linux), a child reaped before the deadline is not signalled and only its pipes are dropped.
/// A deadline that is not in the future returns `TimedOut` without starting the command.
pub async fn run(invocation: &Invocation, deadline: Instant) -> Result<Output, Error> {
    let now = Instant::now();
    if deadline <= now {
        return Err(Error::TimedOut {
            program: invocation.program.clone(),
            timeout: Duration::ZERO,
        });
    }
    let timeout = deadline - now;
    let Started {
        mut leader,
        stdout,
        stderr,
    } = start(invocation).await?;

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

    let cause = match finished {
        Ok(Ok(((), stdout, stderr))) => {
            let status = leader
                .reap()
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
    Err(leader.teardown(invocation, cause).await)
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
    pipe: impl AsyncRead + Unpin,
    stream: Stream,
    invocation: &Invocation,
) -> Result<Vec<u8>, Error> {
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

struct Started {
    leader: Leader,
    stdout: ChildStdout,
    stderr: ChildStderr,
}

/// Starts the reaper thread first, so nothing after the spawn can fail to find the child an owner,
/// then spawns through std so that a `Leader` owns the child the moment the OS process exists:
/// every later setup failure (pidfd, pipe registration) tears the group down and reaps or hands
/// off the leader instead of dropping an unwatched child.
async fn start(invocation: &Invocation) -> Result<Started, Error> {
    let reaper =
        Reaper::start().map_err(|source| invocation.io_error(Operation::StartReaper, source))?;
    let mut command = std::process::Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    #[expect(
        clippy::disallowed_methods,
        reason = "the one spawn behind togi-command's bounded runner; a Leader owns the child before any fallible setup"
    )]
    let spawned = command.spawn();
    let child = spawned.map_err(|source| Error::Spawn {
        program: invocation.program.clone(),
        source,
    })?;
    let mut leader = Leader::new(child, reaper);
    match leader.watch(invocation) {
        Ok((stdout, stderr)) => Ok(Started {
            leader,
            stdout,
            stderr,
        }),
        Err(cause) => Err(leader.teardown(invocation, cause).await),
    }
}

fn async_pipe<S, A>(
    pipe: Option<S>,
    from_std: fn(S) -> io::Result<A>,
    stream: Stream,
    invocation: &Invocation,
) -> Result<A, Error> {
    pipe.ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))
        .and_then(from_std)
        .map_err(|source| invocation.io_error(Operation::Read(stream), source))
}

/// Owns the spawned leader until it is settled: reaped, or handed to its reaper thread. Dropping
/// an unsettled leader kills its group and hands it off, which cannot fail, so neither a running
/// command nor a zombie is left behind on any path.
struct Leader {
    child: Child,
    pid: Pid,
    /// The group still to signal; cleared once it is signalled or the leader is reaped or handed
    /// off, since the id may then be reused.
    group: Option<Pid>,
    /// `None` once the leader is settled; dropping it ends the idle thread.
    reaper: Option<Reaper>,
    #[cfg(target_os = "linux")]
    pidfd: Option<tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>>,
}

impl Leader {
    fn new(child: Child, reaper: Reaper) -> Self {
        let pid = Pid::from_child(&child);
        #[cfg(test)]
        fault::SPAWNED.set(Some(pid));
        Self {
            child,
            pid,
            group: Some(pid),
            reaper: Some(reaper),
            #[cfg(target_os = "linux")]
            pidfd: None,
        }
    }

    fn watch(&mut self, invocation: &Invocation) -> Result<(ChildStdout, ChildStderr), Error> {
        if let Some(source) = injected_setup_failure() {
            return Err(invocation.io_error(Operation::Wait, source));
        }
        #[cfg(target_os = "linux")]
        {
            let pidfd = rustix::process::pidfd_open(self.pid, rustix::process::PidfdFlags::empty())
                .map_err(io::Error::from)
                .and_then(|fd| {
                    tokio::io::unix::AsyncFd::with_interest(fd, tokio::io::Interest::READABLE)
                })
                .map_err(|source| invocation.io_error(Operation::Wait, source))?;
            self.pidfd = Some(pidfd);
        }
        let stdout = async_pipe(
            self.child.stdout.take(),
            ChildStdout::from_std,
            Stream::Stdout,
            invocation,
        )?;
        let stderr = async_pipe(
            self.child.stderr.take(),
            ChildStderr::from_std,
            Stream::Stderr,
            invocation,
        )?;
        Ok((stdout, stderr))
    }

    /// Readiness of the pidfd: the leader has exited and stays an unreaped zombie.
    #[cfg(target_os = "linux")]
    async fn exited(&mut self) -> io::Result<()> {
        match &self.pidfd {
            Some(pidfd) => pidfd.readable().await.map(drop),
            None => Err(io::ErrorKind::NotConnected.into()),
        }
    }

    #[cfg(not(target_os = "linux"))]
    async fn exited(&mut self) -> io::Result<()> {
        while self.try_reap()?.is_none() {
            tokio::time::sleep(EXIT_POLL).await;
        }
        Ok(())
    }

    /// Reaps a leader that has already exited, so the wait returns at once.
    fn reap(&mut self) -> io::Result<ExitStatus> {
        self.group = None;
        self.reaper = None;
        self.child.wait()
    }

    fn try_reap(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait();
        if !matches!(status, Ok(None)) {
            self.group = None;
            self.reaper = None;
        }
        status
    }

    /// Kills the group, then waits up to `REAP_GRACE` for the leader and hands it to a reaper
    /// thread if it is still running. Returns `cause`, or `Error::Teardown` carrying it when the
    /// group could not be signalled or the leader was not reaped in time.
    async fn teardown(&mut self, invocation: &Invocation, cause: Error) -> Error {
        let killed = self
            .kill_group()
            .map_err(|errno| (Operation::KillGroup, io::Error::from(errno)));
        let reaped = self
            .reap_within(REAP_GRACE)
            .await
            .map_err(|source| (Operation::Reap, source));
        match killed.and(reaped) {
            Ok(()) => cause,
            Err((operation, source)) => Error::Teardown {
                program: invocation.program.clone(),
                cause: Box::new(cause),
                operation,
                source,
            },
        }
    }

    async fn reap_within(&mut self, grace: Duration) -> io::Result<()> {
        let give_up = Instant::now() + grace;
        while self.try_reap()?.is_none() {
            if Instant::now() >= give_up {
                self.hand_off();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "the leader did not exit within {grace:?}; a reaper thread waits for it"
                    ),
                ));
            }
            tokio::time::sleep(REAP_POLL).await;
        }
        Ok(())
    }

    /// ESRCH means nothing is left to signal; any other failure keeps the group for a retry on drop.
    fn kill_group(&mut self) -> Result<(), Errno> {
        let Some(group) = self.group else {
            return Ok(());
        };
        match signal_group(group) {
            Ok(()) | Err(Errno::SRCH) => {
                self.group = None;
                Ok(())
            }
            Err(errno) => Err(errno),
        }
    }

    /// Leaves the unreaped leader to its reaper thread, which outlives the caller's runtime, so a
    /// leader that exits late is still collected.
    fn hand_off(&mut self) {
        self.group = None;
        if let Some(reaper) = self.reaper.take() {
            reaper.hand_off(self.pid);
        }
    }
}

impl Drop for Leader {
    /// Cancellation: nobody is left to report a failure to, so kill and reap or hand off.
    fn drop(&mut self) {
        if self.reaper.is_none() {
            return;
        }
        let _ = self.kill_group();
        if matches!(self.try_reap(), Ok(None)) {
            self.hand_off();
        }
    }
}

/// A thread started before the command and blocked until it is given the leader to wait for, so
/// handing a leader off after the spawn cannot fail. Dropping it unused ends the thread.
struct Reaper(mpsc::Sender<Pid>);

impl Reaper {
    fn start() -> io::Result<Self> {
        #[cfg(test)]
        if fault::REAPER.get() {
            return Err(io::Error::other("injected reaper thread failure"));
        }
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("togi-command-reaper".to_owned())
            .spawn(move || {
                if let Ok(pid) = receiver.recv() {
                    reap_blocking(pid);
                }
            })?;
        Ok(Self(sender))
    }

    /// The thread holds the receiver until it is sent a pid or the sender is dropped, and this
    /// consumes the only sender, so the send cannot find the receiver gone.
    fn hand_off(self, pid: Pid) {
        let _ = self.0.send(pid);
    }
}

fn reap_blocking(pid: Pid) {
    while matches!(waitpid(Some(pid), WaitOptions::empty()), Err(Errno::INTR)) {}
}

fn signal_group(group: Pid) -> Result<(), Errno> {
    #[cfg(test)]
    if let Some(fault) = fault::KILL.get() {
        if fault.deliver {
            let _ = kill_process_group(group, Signal::KILL);
        }
        return fault.result;
    }
    kill_process_group(group, Signal::KILL)
}

fn injected_setup_failure() -> Option<io::Error> {
    #[cfg(test)]
    if fault::SETUP.get() {
        return Some(io::Error::other("injected setup failure"));
    }
    None
}

/// Per-thread fault injection for the unit tests: a failed post-spawn setup step, a kill that
/// fails or is not delivered, and a reaper thread that cannot be created.
#[cfg(test)]
mod fault {
    use std::cell::Cell;

    use rustix::io::Errno;
    use rustix::process::Pid;

    #[derive(Debug, Clone, Copy)]
    pub(crate) struct Kill {
        pub(crate) deliver: bool,
        pub(crate) result: Result<(), Errno>,
    }

    thread_local! {
        pub(crate) static SETUP: Cell<bool> = const { Cell::new(false) };
        pub(crate) static KILL: Cell<Option<Kill>> = const { Cell::new(None) };
        pub(crate) static SPAWNED: Cell<Option<Pid>> = const { Cell::new(None) };
        pub(crate) static REAPER: Cell<bool> = const { Cell::new(false) };
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::time::{Duration, Instant as StdInstant};

    use rustix::io::Errno;
    use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process, waitid};

    use super::{Error, Invocation, Operation, Output, REAP_GRACE, fault};

    const DEADLINE: Duration = Duration::from_millis(100);
    const SLACK: Duration = Duration::from_millis(900);

    async fn run_for(invocation: &Invocation, timeout: Duration) -> Result<Output, Error> {
        #[expect(
            clippy::disallowed_methods,
            reason = "tests exercise the bounded runner with real processes"
        )]
        super::run(invocation, tokio::time::Instant::now() + timeout).await
    }

    fn sleep_60() -> Invocation {
        Invocation::new("sleep").arg("60")
    }

    fn spawned() -> Pid {
        fault::SPAWNED.get().expect("a child was spawned")
    }

    /// Whether the runner, or its reaper thread, has reaped our child: `waitid` without `WNOWAIT`
    /// is never called here, so ECHILD can only mean someone in this process collected it.
    fn reaped(pid: Pid) -> bool {
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        match waitid(WaitId::Pid(pid), options) {
            Ok(_) => false,
            Err(Errno::CHILD) => true,
            Err(errno) => panic!("waitid {pid:?}: {errno}"),
        }
    }

    /// Still our child and not exited: the id cannot have been reused.
    fn is_running(pid: Pid) -> bool {
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        matches!(waitid(WaitId::Pid(pid), options), Ok(None))
    }

    fn assert_reaped_within(pid: Pid, within: Duration) {
        let give_up = StdInstant::now() + within;
        while !reaped(pid) {
            assert!(StdInstant::now() < give_up, "{pid:?} was not reaped");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Kills a child the test expects to be running when an assertion fails first. It is armed
    /// only on a child that is still unreaped, which only this guard ends, so its id cannot have
    /// been reused when the guard fires.
    struct KillOnDrop(Pid);

    thread_local! {
        /// Signals sent by `KillOnDrop` guards on this thread.
        static GUARD_SIGNALS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    impl KillOnDrop {
        /// Builds the guard only once the check passes: a guard built and then rejected would
        /// signal the id when dropped.
        fn if_running(pid: Pid) -> Option<Self> {
            if is_running(pid) {
                Some(Self(pid))
            } else {
                None
            }
        }
    }

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            GUARD_SIGNALS.set(GUARD_SIGNALS.get() + 1);
            let _ = kill_process(self.0, Signal::KILL);
        }
    }

    fn assert_kill_failure(source: &std::io::Error) {
        assert_eq!(source.raw_os_error(), Some(Errno::PERM.raw_os_error()));
    }

    #[tokio::test]
    async fn real_process_setup_failure_after_spawn_kills_and_reaps_the_child() {
        fault::SETUP.set(true);
        let started = StdInstant::now();
        let result = run_for(&sleep_60(), Duration::from_secs(60)).await;
        let elapsed = started.elapsed();
        let child = spawned();
        let cleanup = KillOnDrop::if_running(child);
        match &result {
            Err(Error::Io {
                operation: Operation::Wait,
                source,
                ..
            }) => assert_eq!(source.to_string(), "injected setup failure"),
            other => panic!("{other:?}"),
        }
        assert!(elapsed < REAP_GRACE, "{elapsed:?}");
        assert!(cleanup.is_none(), "the child was left running");
        assert!(reaped(child), "the child was left unreaped");
        assert_eq!(
            GUARD_SIGNALS.get(),
            0,
            "a rejected guard signalled a reaped id"
        );
    }

    #[tokio::test]
    async fn real_process_setup_failure_keeps_a_kill_failure_beside_it() {
        fault::SETUP.set(true);
        fault::KILL.set(Some(fault::Kill {
            deliver: false,
            result: Err(Errno::PERM),
        }));
        let started = StdInstant::now();
        let result = run_for(&sleep_60(), Duration::from_secs(60)).await;
        let elapsed = started.elapsed();
        let child = spawned();
        let cleanup = KillOnDrop::if_running(child);
        match &result {
            Err(Error::Teardown {
                cause,
                operation: Operation::KillGroup,
                source,
                ..
            }) => {
                assert!(
                    matches!(
                        **cause,
                        Error::Io {
                            operation: Operation::Wait,
                            ..
                        }
                    ),
                    "{cause:?}"
                );
                assert_kill_failure(source);
            }
            other => panic!("{other:?}"),
        }
        assert!(elapsed < REAP_GRACE + SLACK, "{elapsed:?}");
        assert!(cleanup.is_some(), "the error hid a running group");
        drop(cleanup);
        assert_reaped_within(child, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn real_process_kill_failure_is_reported_with_the_timeout() {
        fault::KILL.set(Some(fault::Kill {
            deliver: false,
            result: Err(Errno::PERM),
        }));
        let started = StdInstant::now();
        let result = run_for(&sleep_60(), DEADLINE).await;
        let elapsed = started.elapsed();
        let child = spawned();
        let cleanup = KillOnDrop::if_running(child);
        let error = result.unwrap_err();
        let message = error.to_string();
        match &error {
            Error::Teardown {
                program,
                cause,
                operation: Operation::KillGroup,
                source,
            } => {
                assert_eq!(program, "sleep");
                assert!(matches!(**cause, Error::TimedOut { .. }), "{cause:?}");
                assert_kill_failure(source);
            }
            other => panic!("{other:?}"),
        }
        assert!(
            message.contains("did not finish") && message.contains("kill the process group"),
            "{message}"
        );
        assert!(elapsed < DEADLINE + REAP_GRACE + SLACK, "{elapsed:?}");
        assert!(cleanup.is_some(), "the error hid a running group");
        drop(cleanup);
        assert_reaped_within(child, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn real_process_kill_finding_no_group_is_harmless() {
        fault::KILL.set(Some(fault::Kill {
            deliver: true,
            result: Err(Errno::SRCH),
        }));
        let result = run_for(&sleep_60(), DEADLINE).await;
        let child = spawned();
        let cleanup = KillOnDrop::if_running(child);
        assert!(matches!(result, Err(Error::TimedOut { .. })), "{result:?}");
        assert!(cleanup.is_none(), "the child was left running");
        assert!(reaped(child), "the child was left unreaped");
        assert_eq!(
            GUARD_SIGNALS.get(),
            0,
            "a rejected guard signalled a reaped id"
        );
    }

    /// A leader that ignores SIGKILL for a while (in the field: uninterruptible sleep) is simulated
    /// by not delivering the kill; it outlives the grace and the per-call runtime.
    #[test]
    fn real_process_leader_outliving_the_reap_grace_is_collected_after_the_runtime_is_gone() {
        fault::KILL.set(Some(fault::Kill {
            deliver: false,
            result: Ok(()),
        }));
        let started = StdInstant::now();
        #[expect(
            clippy::disallowed_methods,
            reason = "tests exercise the bounded runner with real processes"
        )]
        let error = super::run_blocking(&Invocation::new("sleep").arg("3"), DEADLINE).unwrap_err();
        let elapsed = started.elapsed();
        let child = spawned();
        match &error {
            Error::Teardown {
                cause,
                operation: Operation::Reap,
                source,
                ..
            } => {
                assert!(matches!(**cause, Error::TimedOut { .. }), "{cause:?}");
                assert_eq!(source.kind(), std::io::ErrorKind::TimedOut);
            }
            other => panic!("{other:?}"),
        }
        assert!(elapsed < DEADLINE + REAP_GRACE + SLACK, "{elapsed:?}");
        assert!(is_running(child), "the leader exited within the grace");
        assert_reaped_within(child, Duration::from_secs(10));
    }

    /// Cancellation has no grace and no caller: dropping the future hands a leader the kill did not
    /// reach straight to the reaper thread, which collects it once it exits.
    #[test]
    fn real_process_cancelled_run_hands_a_surviving_leader_to_the_reaper() {
        fault::KILL.set(Some(fault::Kill {
            deliver: false,
            result: Ok(()),
        }));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let started = StdInstant::now();
        let abandoned = runtime.block_on(async {
            tokio::time::timeout(
                DEADLINE,
                run_for(&Invocation::new("sleep").arg("3"), Duration::from_secs(60)),
            )
            .await
        });
        drop(runtime);
        let elapsed = started.elapsed();
        let child = spawned();
        assert!(abandoned.is_err(), "{abandoned:?}");
        assert!(elapsed < DEADLINE + SLACK, "{elapsed:?}");
        assert!(
            is_running(child),
            "the leader exited before it was handed off"
        );
        assert_reaped_within(child, Duration::from_secs(10));
    }

    /// The reaper thread starts before the command, so a thread that cannot be created means no
    /// process was started.
    #[test]
    fn a_reaper_thread_that_cannot_start_starts_nothing() {
        fault::REAPER.set(true);
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let invocation = Invocation::new("sh")
            .arg("-c")
            .arg(format!("touch {}", marker.display()));
        let started = StdInstant::now();
        #[expect(
            clippy::disallowed_methods,
            reason = "tests exercise the bounded runner with real processes"
        )]
        let result = super::run_blocking(&invocation, Duration::from_secs(10));
        let elapsed = started.elapsed();
        match &result {
            Err(Error::Io {
                program,
                operation: Operation::StartReaper,
                source,
            }) => {
                assert_eq!(program, "sh");
                assert_eq!(source.to_string(), "injected reaper thread failure");
            }
            other => panic!("{other:?}"),
        }
        assert!(elapsed < Duration::from_millis(100), "{elapsed:?}");
        assert_eq!(fault::SPAWNED.get(), None, "a process was started");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!marker.exists(), "the command ran");
    }
}
