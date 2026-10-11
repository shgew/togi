use std::ffi::OsString;
use std::io;
use std::os::fd::OwnedFd;

use rustix::process::{
    Pid, PidfdFlags, Signal, WaitId, WaitIdOptions, kill_process_group, pidfd_open, waitid,
};
use togi_machine::CpuId;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::net::unix::pipe::Receiver;
use tokio::time::Instant;

use super::{Child, ExitInfo, GroupSignal, Os, Pipes, SignalError, Systemd};
use crate::id::ScopeName;
use crate::launch::{LaunchCommand, scope_argv};
use crate::procfs::{Proc, ProcError, ScopeProcess, Threads, Usage};

/// The leader: a std child plus its pidfd, observed with `waitid(P_PIDFD, WNOWAIT)` so the zombie
/// keeps its pid and process group until teardown reaps it.
pub(crate) struct Leader {
    child: std::process::Child,
    pidfd: AsyncFd<OwnedFd>,
    pid: u32,
    group: Pid,
}

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
        self.pidfd.readable().await.map(drop)
    }

    fn exit(&self) -> io::Result<Option<ExitInfo>> {
        let status = waitid(
            WaitId::PidFd(std::os::fd::AsFd::as_fd(self.pidfd.get_ref())),
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
}

impl Linux {
    pub(crate) fn new(proc: Proc, user_manager: bool) -> Self {
        Self {
            proc,
            user_manager,
            systemctl: "systemctl".into(),
        }
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

    fn launch(&self, command: &LaunchCommand) -> io::Result<Leader> {
        let mut child = crate::launch::spawn(command)?;
        let pid = child.id();
        let group = i32::try_from(pid).ok().and_then(Pid::from_raw);
        let opened = group
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))
            .and_then(|group| {
                let fd = pidfd_open(group, PidfdFlags::empty())?;
                Ok((group, AsyncFd::with_interest(fd, Interest::READABLE)?))
            });
        match opened {
            Ok((group, pidfd)) => Ok(Leader {
                child,
                pidfd,
                pid,
                group,
            }),
            Err(error) => {
                if let Some(group) = group {
                    let _ = kill_process_group(group, Signal::KILL);
                }
                let _ = child.wait();
                Err(error)
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

    fn scope_processes(&self) -> Result<Vec<ScopeProcess>, ProcError> {
        self.proc.scope_processes()
    }

    fn process_alive(&self, process: &ScopeProcess) -> Result<bool, ProcError> {
        self.proc.alive(process)
    }
}
