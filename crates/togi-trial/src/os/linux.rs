use std::io;

use tokio::time::Instant;

use super::{Child, ExitInfo, GroupSignal, Os, Pipes, SignalError, Systemctl};
use crate::id::ScopeName;
use crate::launch::LaunchCommand;
use crate::procfs::{Proc, ProcError, ScopeProcess, Threads, Usage};

/// The leader: a std child plus its pidfd, observed with `waitid(P_PIDFD, WNOWAIT)`.
pub(crate) struct Leader {
    pid: u32,
}

impl Child for Leader {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn take_pipes(&mut self) -> io::Result<Pipes> {
        unimplemented!()
    }

    async fn exited(&self) -> io::Result<()> {
        unimplemented!()
    }

    fn exit(&self) -> io::Result<Option<ExitInfo>> {
        unimplemented!()
    }

    fn signal_group(&self, signal: GroupSignal) -> Result<(), SignalError> {
        let _ = signal;
        unimplemented!()
    }

    fn reap(self) -> io::Result<ExitInfo> {
        unimplemented!()
    }
}

pub(crate) struct Linux {
    proc: Proc,
    user_manager: bool,
}

impl Linux {
    pub(crate) fn new(proc: Proc, user_manager: bool) -> Self {
        Self { proc, user_manager }
    }
}

impl Os for Linux {
    type Child = Leader;

    fn launch(&self, command: &LaunchCommand) -> io::Result<Leader> {
        let _ = command;
        unimplemented!()
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

    async fn systemctl(
        &self,
        operation: &Systemctl,
        deadline: Instant,
    ) -> Result<togi_command::Output, togi_command::Error> {
        let _ = (operation, deadline, self.user_manager);
        unimplemented!()
    }

    fn scope_processes(&self) -> Result<Vec<ScopeProcess>, ProcError> {
        self.proc.scope_processes()
    }

    fn process_alive(&self, process: &ScopeProcess) -> Result<bool, ProcError> {
        self.proc.alive(process)
    }
}
