use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use rustix::process::{
    Pid, PidfdFlags, Signal, WaitId, WaitIdOptions, kill_process_group, pidfd_open, waitid,
};
use togi_machine::CpuId;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::net::unix::pipe::Receiver;
use tokio::time::Instant;

use super::{Child, ExitInfo, GroupSignal, LaunchFailure, Os, Pipes, SignalError, Systemd};
use crate::id::ScopeName;
use crate::launch::{LaunchCommand, scope_argv};
use crate::procfs::{Proc, ProcError, ScopeProcess, Threads, Usage};

/// The leader: a std child plus its pidfd, observed with `waitid(P_PIDFD, WNOWAIT)` so the zombie
/// keeps its pid and process group until teardown reaps it.
pub(crate) struct Leader {
    child: std::process::Child,
    watch: ExitWatch,
    pid: u32,
    group: Pid,
}

/// How the leader's exit is observed: by its pidfd, or, when that could not be set up, by polling
/// `waitid(P_PID, WNOWAIT)` so the rollback can still confirm and reap it within its budget.
enum ExitWatch {
    Pidfd(AsyncFd<OwnedFd>),
    Poll,
}

const EXIT_POLL: std::time::Duration = std::time::Duration::from_millis(50);

fn io_pipe(file: impl Into<OwnedFd>) -> io::Result<super::Pipe> {
    let receiver = Receiver::from_file(std::fs::File::from(file.into()))?;
    Ok(Box::pin(receiver))
}

impl Child for Leader {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn take_pipes(&mut self) -> io::Result<Pipes> {
        let missing = || io::Error::from(io::ErrorKind::NotConnected);
        let stdout = self.child.stdout.take().ok_or_else(missing)?;
        let stderr = self.child.stderr.take().ok_or_else(missing)?;
        Ok(Pipes {
            stdout: io_pipe(stdout)?,
            stderr: io_pipe(stderr)?,
        })
    }

    async fn exited(&self) -> io::Result<()> {
        match &self.watch {
            ExitWatch::Pidfd(pidfd) => pidfd.readable().await.map(drop),
            ExitWatch::Poll => loop {
                if self.exit()?.is_some() {
                    return Ok(());
                }
                tokio::time::sleep(EXIT_POLL).await;
            },
        }
    }

    fn exit(&self) -> io::Result<Option<ExitInfo>> {
        let id = match &self.watch {
            ExitWatch::Pidfd(pidfd) => WaitId::PidFd(std::os::fd::AsFd::as_fd(pidfd.get_ref())),
            ExitWatch::Poll => WaitId::Pid(self.group),
        };
        let status = waitid(
            id,
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
        )?;
        Ok(status.map(|status| {
            status
                .exit_status()
                .map(ExitInfo::Code)
                .or_else(|| status.terminating_signal().map(ExitInfo::Signal))
                .unwrap_or(ExitInfo::Code(-1))
        }))
    }

    fn signal_group(&self, signal: GroupSignal) -> Result<(), SignalError> {
        let signal = match signal {
            GroupSignal::Cont => Signal::CONT,
            GroupSignal::Term => Signal::TERM,
            GroupSignal::Kill => Signal::KILL,
            GroupSignal::Stop => Signal::STOP,
        };
        match kill_process_group(self.group, signal) {
            Ok(()) => Ok(()),
            Err(rustix::io::Errno::SRCH) => Err(SignalError::Gone),
            Err(errno) => Err(SignalError::Failed(errno.into())),
        }
    }

    fn reap(mut self) -> io::Result<ExitInfo> {
        if self.exit()?.is_none() {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        self.child.wait().map(ExitInfo::from)
    }
}

pub(crate) struct Linux {
    proc: Proc,
    user_manager: bool,
    systemctl: OsString,
    scans: Arc<Scans>,
    #[cfg(test)]
    pidfd_refused: bool,
}

impl Linux {
    pub(crate) fn new(proc: Proc, user_manager: bool) -> Self {
        Self {
            proc,
            user_manager,
            systemctl: "systemctl".into(),
            scans: Arc::default(),
            #[cfg(test)]
            pidfd_refused: false,
        }
    }

    /// Test construction: `pidfd_open` fails, so a launch takes the polling exit watch.
    #[cfg(test)]
    pub(crate) fn with_pidfd_refused(mut self) -> Self {
        self.pidfd_refused = true;
        self
    }

    /// Test construction: a stand-in for the `systemctl` binary.
    #[cfg(test)]
    pub(crate) fn with_systemctl(mut self, program: impl Into<OsString>) -> Self {
        self.systemctl = program.into();
        self
    }

    fn argv(&self, operation: &Systemd) -> Vec<OsString> {
        let mut argv: Vec<OsString> = Vec::new();
        let mut systemctl = |arguments: &[String]| {
            argv.push(self.systemctl.clone());
            if self.user_manager {
                argv.push("--user".into());
            }
            argv.extend(arguments.iter().map(OsString::from));
        };
        match operation {
            Systemd::Kill { scope, signal } => systemctl(&[
                "kill".to_owned(),
                format!("--signal={}", signal.name()),
                "--kill-whom=all".to_owned(),
                scope.unit(),
            ]),
            Systemd::Stop { scope } => systemctl(&["stop".to_owned(), scope.unit()]),
            Systemd::ListScopes => systemctl(
                &[
                    "list-units",
                    "--all",
                    "--type=scope",
                    "--no-legend",
                    "--plain",
                    "--no-pager",
                    "togi-trial-*.scope",
                ]
                .map(str::to_owned),
            ),
            Systemd::Probe { unit, identity } => {
                return scope_argv(
                    unit,
                    *identity,
                    self.user_manager,
                    std::path::Path::new("/"),
                    &[CpuId(0)],
                    &["/bin/sh".into(), "-c".into(), "exit 0".into()],
                );
            }
        }
        argv
    }
}

impl Os for Linux {
    type Child = Leader;

    fn launch(&self, command: &LaunchCommand) -> Result<Leader, LaunchFailure<Leader>> {
        let child = crate::launch::spawn(command)?;
        let pid = child.id();
        let Some(group) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
            return Err(io::Error::from(io::ErrorKind::InvalidData).into());
        };
        #[cfg(test)]
        let refused = self.pidfd_refused;
        #[cfg(not(test))]
        let refused = false;
        let opened = if refused {
            Err(io::Error::other("pidfd_open refused"))
        } else {
            pidfd_open(group, PidfdFlags::empty()).map_err(io::Error::from)
        }
        .and_then(|fd| AsyncFd::with_interest(fd, Interest::READABLE));
        match opened {
            Ok(pidfd) => Ok(Leader {
                child,
                watch: ExitWatch::Pidfd(pidfd),
                pid,
                group,
            }),
            Err(source) => {
                let _ = kill_process_group(group, Signal::KILL);
                Err(LaunchFailure {
                    child: Some(Leader {
                        child,
                        watch: ExitWatch::Poll,
                        pid,
                        group,
                    }),
                    source,
                })
            }
        }
    }

    fn in_scope(&self, pid: u32, scope: &ScopeName) -> Result<bool, ProcError> {
        self.proc.in_scope(pid, scope)
    }

    fn usage(&self, pid: u32) -> Result<Usage, ProcError> {
        self.proc.usage(pid)
    }

    fn threads(&self, pid: u32) -> Result<Threads, ProcError> {
        self.proc.threads(pid)
    }

    async fn systemd(
        &self,
        operation: &Systemd,
        deadline: Instant,
    ) -> Result<togi_command::Output, togi_command::Error> {
        let argv = self.argv(operation);
        let (program, arguments) = argv.split_first().map_or_else(
            || (OsString::new(), Vec::new()),
            |(program, rest)| (program.clone(), rest.to_vec()),
        );
        let invocation = togi_command::Invocation::new(program).args(arguments);
        #[expect(
            clippy::disallowed_methods,
            reason = "systemctl and systemd-run are bounded by togi_command's deadline and group kill"
        )]
        togi_command::run(&invocation, deadline).await
    }

    async fn scope_processes(&self, deadline: Instant) -> Result<Vec<ScopeProcess>, ProcError> {
        let proc = self.proc.clone();
        self.scans
            .run(deadline, move || proc.scope_processes())
            .await
    }

    async fn survivors(
        &self,
        captured: &[ScopeProcess],
        deadline: Instant,
    ) -> Result<usize, ProcError> {
        let (proc, captured) = (self.proc.clone(), captured.to_vec());
        self.scans
            .run(deadline, move || {
                let mut alive = 0;
                for process in &captured {
                    if proc.alive(process)? {
                        alive += 1;
                    }
                }
                Ok(alive)
            })
            .await
    }
}

const RUNNING: u8 = 0;
const ABANDONED: u8 = 1;
const DONE: u8 = 2;

/// The `/proc` scans of one `Linux`, shared by its users: how many scans were abandoned at their
/// deadline or by their cancelled caller and are still blocked on the kernel.
#[derive(Default)]
struct Scans {
    abandoned: AtomicUsize,
}

/// Marks one scan finished, whether its closure returned, panicked or was dropped unrun, and
/// clears its share of the abandoned count if the runtime had already given up on it.
struct Finished {
    state: Arc<AtomicU8>,
    scans: Arc<Scans>,
}

impl Drop for Finished {
    fn drop(&mut self) {
        if self.state.swap(DONE, Ordering::AcqRel) == ABANDONED {
            self.scans.abandoned.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// Abandons the scan when its caller stops awaiting it, at the deadline or by cancellation, unless
/// the scan already finished.
struct Awaiting<'a> {
    state: &'a AtomicU8,
    scans: &'a Scans,
}

impl Drop for Awaiting<'_> {
    fn drop(&mut self) {
        if self.state.load(Ordering::Acquire) != RUNNING {
            return;
        }
        self.scans.abandoned.fetch_add(1, Ordering::AcqRel);
        if self
            .state
            .compare_exchange(RUNNING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            self.scans.abandoned.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Scans {
    /// Runs a `/proc` scan on tokio's blocking pool, so a slow read cannot stall the runtime
    /// thread, and gives up at `deadline` however long the pool queues it. A scan abandoned at its
    /// deadline or by its cancelled caller keeps its blocking thread until the kernel returns;
    /// while any such scan is still running no new one is started, so a stuck `/proc` cannot pile
    /// up threads.
    async fn run<T: Send + 'static>(
        self: &Arc<Self>,
        deadline: Instant,
        scan: impl FnOnce() -> Result<T, ProcError> + Send + 'static,
    ) -> Result<T, ProcError> {
        if self.abandoned.load(Ordering::Acquire) > 0 {
            return Err(ProcError::Stuck);
        }
        let state = Arc::new(AtomicU8::new(RUNNING));
        let _awaiting = Awaiting {
            state: &state,
            scans: self,
        };
        let finished = Finished {
            state: Arc::clone(&state),
            scans: Arc::clone(self),
        };
        let work = tokio::task::spawn_blocking(move || {
            let _finished = finished;
            scan()
        });
        match tokio::time::timeout_at(deadline, work).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(ProcError::ScanLost),
            Err(_) => Err(ProcError::Deadline),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Future as _, poll_fn};
    use std::task::Poll;
    use std::time::Duration;

    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn current_thread() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn linux() -> Linux {
        Linux::new(
            Proc::new("/proc".into(), crate::procfs::ticks_per_second()),
            false,
        )
    }

    #[test]
    fn real_process_a_launcher_whose_pidfd_could_not_be_opened_is_killed_polled_and_reaped() {
        let os = linux().with_pidfd_refused();
        let command = LaunchCommand {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
            cwd: "/".into(),
            pin: Vec::new(),
        };
        current_thread().block_on(async {
            let begun = std::time::Instant::now();
            let failure = os.launch(&command).err().unwrap();
            assert!(begun.elapsed() < secs(5), "launch waited for the launcher");
            assert_eq!(failure.source.to_string(), "pidfd_open refused");
            let leader = failure.child.expect("the spawned launcher stays owned");
            assert!(matches!(leader.watch, ExitWatch::Poll));
            tokio::time::timeout(secs(10), leader.exited())
                .await
                .expect("the polling watch sees the group kill")
                .unwrap();
            assert_eq!(leader.exit().unwrap(), Some(ExitInfo::Signal(9)));
            assert_eq!(leader.reap().unwrap(), ExitInfo::Signal(9));
        });
    }

    fn scans() -> Arc<Scans> {
        Arc::default()
    }

    #[test]
    fn real_process_a_cancelled_scan_refuses_replacements_until_its_owner_finishes() {
        for abort in [false, true] {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .max_blocking_threads(1)
                .enable_all()
                .build()
                .unwrap();
            let scans = scans();
            let (release, blocked) = std::sync::mpsc::channel::<()>();
            let (started, running) = tokio::sync::oneshot::channel();
            runtime.block_on(async move {
                let owner_scans = Arc::clone(&scans);
                let awaiting = async move {
                    owner_scans
                        .run(Instant::now() + secs(60), move || {
                            let _ = started.send(());
                            blocked.recv().unwrap();
                            Ok(())
                        })
                        .await
                };
                if abort {
                    let owner = tokio::spawn(awaiting);
                    running.await.unwrap();
                    owner.abort();
                    assert!(owner.await.unwrap_err().is_cancelled());
                } else {
                    let mut owner = Box::pin(awaiting);
                    tokio::select! {
                        () = async { running.await.unwrap() } => {}
                        outcome = &mut owner => panic!("the owner was not blocked: {outcome:?}"),
                    }
                    drop(owner);
                }
                let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let flag = Arc::clone(&ran);
                let mut replacement = Box::pin(scans.run(Instant::now() + secs(60), move || {
                    flag.store(true, Ordering::SeqCst);
                    Ok(())
                }));
                let refused = poll_fn(|cx| {
                    Poll::Ready(match replacement.as_mut().poll(cx) {
                        Poll::Ready(result) => Some(result),
                        Poll::Pending => None,
                    })
                })
                .await;
                drop(replacement);
                let abandoned = scans.abandoned.load(Ordering::Acquire);
                release.send(()).unwrap();
                tokio::task::spawn_blocking(|| {}).await.unwrap();
                let finished = scans.abandoned.load(Ordering::Acquire);
                let resumed = scans.run(Instant::now() + secs(60), || Ok(7)).await;
                assert_eq!(refused, Some(Err(ProcError::Stuck)), "abort: {abort}");
                assert!(!ran.load(Ordering::SeqCst), "abort: {abort}");
                assert_eq!(abandoned, 1, "abort: {abort}");
                assert_eq!(finished, 0, "abort: {abort}");
                assert_eq!(resumed, Ok(7), "abort: {abort}");
            });
        }
    }

    #[test]
    fn real_process_cancelling_a_finished_scan_does_not_abandon_it() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let scans = scans();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let (started, running) = tokio::sync::oneshot::channel();
        runtime.block_on(async move {
            let mut owner = Box::pin(scans.run(Instant::now() + secs(60), move || {
                let _ = started.send(());
                blocked.recv().unwrap();
                Ok(())
            }));
            tokio::select! {
                () = async { running.await.unwrap() } => {}
                outcome = &mut owner => panic!("the owner was not blocked: {outcome:?}"),
            }
            release.send(()).unwrap();
            tokio::task::spawn_blocking(|| {}).await.unwrap();
            assert_eq!(scans.abandoned.load(Ordering::Acquire), 0);
            drop(owner);
            assert_eq!(scans.abandoned.load(Ordering::Acquire), 0);
            assert_eq!(scans.run(Instant::now() + secs(60), || Ok(7)).await, Ok(7));
        });
    }

    #[test]
    fn real_process_a_timed_out_scan_is_counted_once_and_released_once() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let scans = scans();
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let (started, running) = tokio::sync::oneshot::channel();
        runtime.block_on(async move {
            let mut owner = Box::pin(scans.run(
                Instant::now() + Duration::from_millis(100),
                move || {
                    let _ = started.send(());
                    blocked.recv().unwrap();
                    Ok(())
                },
            ));
            let timed_out = owner.as_mut().await;
            running.await.unwrap();
            let before_drop = scans.abandoned.load(Ordering::Acquire);
            drop(owner);
            let after_drop = scans.abandoned.load(Ordering::Acquire);
            let refused = scans.run(Instant::now() + secs(60), || Ok(())).await;
            release.send(()).unwrap();
            tokio::task::spawn_blocking(|| {}).await.unwrap();
            let finished = scans.abandoned.load(Ordering::Acquire);
            let resumed = scans.run(Instant::now() + secs(60), || Ok(7)).await;
            assert_eq!(timed_out, Err(ProcError::Deadline));
            assert_eq!(before_drop, 1);
            assert_eq!(after_drop, 1);
            assert_eq!(refused, Err(ProcError::Stuck));
            assert_eq!(finished, 0);
            assert_eq!(resumed, Ok(7));
        });
    }

    #[test]
    fn real_process_a_blocked_scan_neither_stalls_the_runtime_nor_outlives_its_deadline() {
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let scans = scans();
        let runtime = current_thread();
        let (ticks, elapsed, outcome) = runtime.block_on(async {
            let begun = std::time::Instant::now();
            let ticks = std::cell::Cell::new(0_u32);
            let ticker = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    ticks.set(ticks.get() + 1);
                }
            };
            let scan = scans.run(Instant::now() + Duration::from_millis(300), move || {
                let _ = blocked.recv_timeout(secs(30));
                Ok(())
            });
            let outcome = tokio::select! {
                outcome = scan => outcome,
                () = ticker => unreachable!("the ticker never finishes"),
            };
            (ticks.get(), begun.elapsed(), outcome)
        });
        drop(release);
        drop(runtime);
        assert_eq!(outcome, Err(ProcError::Deadline));
        assert!(
            ticks >= 5,
            "the runtime made progress during the scan: {ticks}"
        );
        assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
        assert!(
            elapsed < secs(5),
            "the wait ended at the deadline: {elapsed:?}"
        );
    }

    #[test]
    fn real_process_a_saturated_blocking_pool_cannot_extend_a_scan_deadline() {
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let scans = scans();
        let (elapsed, outcome) = runtime.block_on(async {
            let (started, running) = tokio::sync::oneshot::channel();
            let hog = tokio::task::spawn_blocking(move || {
                let _ = started.send(());
                let _ = blocked.recv_timeout(secs(30));
            });
            running.await.unwrap();
            let begun = std::time::Instant::now();
            let outcome = scans
                .run(Instant::now() + Duration::from_millis(300), || Ok(()))
                .await;
            let elapsed = begun.elapsed();
            let _ = release.send(());
            hog.await.unwrap();
            (elapsed, outcome)
        });
        assert_eq!(outcome, Err(ProcError::Deadline));
        assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
        assert!(
            elapsed < secs(5),
            "queue time extended the bound: {elapsed:?}"
        );
    }

    #[test]
    fn real_process_a_stuck_scan_refuses_new_scans_until_it_returns() {
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let scans = scans();
        runtime.block_on(async {
            let stuck = scans
                .run(Instant::now() + Duration::from_millis(100), move || {
                    let _ = blocked.recv_timeout(secs(30));
                    Ok(())
                })
                .await;
            assert_eq!(stuck, Err(ProcError::Deadline));
            let begun = std::time::Instant::now();
            let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = Arc::clone(&ran);
            let refused = scans
                .run(Instant::now() + secs(10), move || {
                    flag.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .await;
            assert_eq!(refused, Err(ProcError::Stuck));
            assert!(begun.elapsed() < secs(1), "the refusal was immediate");
            assert!(!ran.load(Ordering::SeqCst));
            release.send(()).unwrap();
            tokio::task::spawn_blocking(|| {}).await.unwrap();
            let again = scans.run(Instant::now() + secs(10), || Ok(7)).await;
            assert_eq!(again, Ok(7));
        });
    }

    #[test]
    fn real_process_healthy_scans_run_concurrently() {
        let scans = scans();
        let meet = Arc::new(std::sync::Barrier::new(2));
        let both = current_thread().block_on(async {
            let scan = |meet: Arc<std::sync::Barrier>| {
                scans.run(Instant::now() + secs(10), move || {
                    meet.wait();
                    Ok(())
                })
            };
            tokio::join!(scan(Arc::clone(&meet)), scan(meet))
        });
        assert_eq!(both, (Ok(()), Ok(())));
    }

    #[test]
    fn real_process_a_scan_that_panics_is_a_lost_scan_and_does_not_block_the_next() {
        let scans = scans();
        let (lost, next) = current_thread().block_on(async {
            let lost = scans
                .run::<()>(Instant::now() + secs(10), || panic!("scan failed"))
                .await;
            let next = scans.run(Instant::now() + secs(10), || Ok(1)).await;
            (lost, next)
        });
        assert_eq!(lost, Err(ProcError::ScanLost));
        assert_eq!(next, Ok(1));
    }
}
