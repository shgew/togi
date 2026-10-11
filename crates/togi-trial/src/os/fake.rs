//! The scoped fake: readiness follows scripted cgroup membership, identity is valid, and every
//! systemd call is recorded and answered from a script. Time is tokio's, so tests pause it.
//!
//! A launched process is a script: an async block holding the pipe writers and receiving every
//! signal, free to print, sleep, ignore `SIGTERM` or hold the pipes after its leader exited.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::pin::Pin;
use std::process::ExitStatus;
use std::rc::Rc;
use std::time::Duration;

use tokio::io::{AsyncWriteExt as _, DuplexStream, duplex};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;
use tokio::time::{Instant, sleep_until};

use super::{Child, ExitInfo, GroupSignal, KillSignal, Os, Pipes, SignalError, Systemd};
use crate::id::ScopeName;
use crate::launch::LaunchCommand;
use crate::procfs::{ProcError, ScopeProcess, Threads, Usage};

/// How a signal reached a fake process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Delivery {
    Group(GroupSignal),
    /// `systemctl kill` on the process's scope.
    Scope(KillSignal),
}

pub(crate) type ProcessScript = Box<dyn FnOnce(ProcCtx) -> Pin<Box<dyn Future<Output = ()>>>>;

pub(crate) enum Launch {
    Run(ProcessScript),
    Fail(io::Error),
}

/// What the fake systemd answers to one call.
pub(crate) enum Reply {
    Output { success: bool, text: String },
    /// Never answers: the call runs to its deadline and times out.
    Hang,
    Fail(togi_command::Error),
}

pub(crate) struct ProcShared {
    pub(crate) pid: u32,
    pub(crate) scope: Option<String>,
    pub(crate) ready: Cell<bool>,
    pub(crate) usage: RefCell<Result<Usage, ProcError>>,
    pub(crate) threads: RefCell<Result<Threads, ProcError>>,
    pub(crate) signals: RefCell<Vec<GroupSignal>>,
    pub(crate) signal_error: RefCell<Option<SignalErrorKind>>,
    pub(crate) reaped: Cell<bool>,
    exit: Cell<Option<ExitInfo>>,
    exited: watch::Sender<bool>,
    to_script: UnboundedSender<Delivery>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum SignalErrorKind {
    Gone,
    Failed,
}

impl ProcShared {
    pub(crate) fn exited(&self) -> bool {
        self.exit.get().is_some()
    }

    pub(crate) fn exit_with(&self, info: ExitInfo) {
        if self.exit.get().is_none() {
            self.exit.set(Some(info));
            self.exited.send_replace(true);
        }
    }
}

/// What a process script holds.
pub(crate) struct ProcCtx {
    pub(crate) shared: Rc<ProcShared>,
    pub(crate) stdout: DuplexStream,
    pub(crate) stderr: DuplexStream,
    pub(crate) signals: UnboundedReceiver<Delivery>,
}

impl ProcCtx {
    pub(crate) async fn print(&mut self, text: &str) {
        let _ = self.stdout.write_all(text.as_bytes()).await;
    }

    pub(crate) async fn eprint(&mut self, text: &str) {
        let _ = self.stderr.write_all(text.as_bytes()).await;
    }

    pub(crate) fn exit(&self, info: ExitInfo) {
        self.shared.exit_with(info);
    }

    /// Runs until `SIGTERM` or `SIGKILL` reaches the group, or a scope kill reaches the scope;
    /// the leader then exits.
    pub(crate) async fn until_killed(&mut self) {
        self.until(|delivery| {
            matches!(
                delivery,
                Delivery::Group(GroupSignal::Term | GroupSignal::Kill) | Delivery::Scope(_)
            )
        })
        .await;
    }

    /// Ignores `SIGTERM`: only `SIGKILL` of the group or a scope kill ends it.
    pub(crate) async fn until_killed_ignoring_term(&mut self) {
        self.until(|delivery| {
            matches!(delivery, Delivery::Group(GroupSignal::Kill) | Delivery::Scope(_))
        })
        .await;
    }

    /// Survives its group's death and ends only on a scope kill, like a detached descendant.
    pub(crate) async fn until_scope_killed(&mut self) {
        while let Some(delivery) = self.signals.recv().await {
            if matches!(delivery, Delivery::Scope(_)) {
                break;
            }
        }
    }

    async fn until(&mut self, mut ends: impl FnMut(Delivery) -> bool) {
        while let Some(delivery) = self.signals.recv().await {
            if ends(delivery) {
                self.exit(match delivery {
                    Delivery::Group(GroupSignal::Term) => ExitInfo::Signal(15),
                    _ => ExitInfo::Signal(9),
                });
                return;
            }
        }
    }
}

type LaunchScript = Box<dyn FnMut(usize, &LaunchCommand) -> Launch>;
type SystemdScript = Box<dyn FnMut(&Systemd) -> Option<Reply>>;

struct Shared {
    launches: RefCell<Vec<LaunchCommand>>,
    processes: RefCell<Vec<Rc<ProcShared>>>,
    systemd_calls: RefCell<Vec<(Systemd, Instant)>>,
    launch_script: RefCell<LaunchScript>,
    systemd_script: RefCell<SystemdScript>,
    units: RefCell<Vec<ScopeName>>,
    recovered: RefCell<Vec<(ScopeProcess, Rc<Cell<bool>>)>>,
    next_pid: Cell<u32>,
}

/// The fake OS; clones share one state, so a test keeps a handle after giving one to a runner.
#[derive(Clone)]
pub(crate) struct Fake {
    shared: Rc<Shared>,
}

impl Default for Fake {
    fn default() -> Self {
        Self {
            shared: Rc::new(Shared {
                launches: RefCell::default(),
                processes: RefCell::default(),
                systemd_calls: RefCell::default(),
                launch_script: RefCell::new(Box::new(|_, _| {
                    Launch::Run(script(|mut ctx| async move { ctx.until_killed().await }))
                })),
                systemd_script: RefCell::new(Box::new(|_| None)),
                units: RefCell::default(),
                recovered: RefCell::default(),
                next_pid: Cell::new(1000),
            }),
        }
    }
}

/// Boxes an async closure as a process script.
pub(crate) fn script<F, Fut>(run: F) -> ProcessScript
where
    F: FnOnce(ProcCtx) -> Fut + 'static,
    Fut: Future<Output = ()> + 'static,
{
    Box::new(move |ctx| Box::pin(run(ctx)))
}

fn scope_in(command: &LaunchCommand) -> Option<String> {
    let position = command.argv.iter().position(|arg| arg == "--unit")?;
    Some(command.argv.get(position + 1)?.to_string_lossy().into_owned())
}

impl Fake {
    pub(crate) fn on_launch(&self, script: impl FnMut(usize, &LaunchCommand) -> Launch + 'static) {
        *self.shared.launch_script.borrow_mut() = Box::new(script);
    }

    /// Answers systemd calls; `None` falls back to the default (success, effect applied).
    pub(crate) fn on_systemd(&self, script: impl FnMut(&Systemd) -> Option<Reply> + 'static) {
        *self.shared.systemd_script.borrow_mut() = Box::new(script);
    }

    pub(crate) fn process(&self, index: usize) -> Rc<ProcShared> {
        Rc::clone(&self.shared.processes.borrow()[index])
    }

    pub(crate) fn process_count(&self) -> usize {
        self.shared.processes.borrow().len()
    }

    pub(crate) fn launches(&self) -> Vec<LaunchCommand> {
        self.shared.launches.borrow().clone()
    }

    pub(crate) fn systemd_calls(&self) -> Vec<(Systemd, Instant)> {
        self.shared.systemd_calls.borrow().clone()
    }

    pub(crate) fn add_unit(&self, scope: ScopeName) {
        self.shared.units.borrow_mut().push(scope);
    }

    pub(crate) fn units(&self) -> Vec<ScopeName> {
        self.shared.units.borrow().clone()
    }

    /// A process found in a scope by `/proc` discovery; the returned flag says whether it lives.
    pub(crate) fn add_recovered(&self, process: ScopeProcess) -> Rc<Cell<bool>> {
        let alive = Rc::new(Cell::new(true));
        self.shared
            .recovered
            .borrow_mut()
            .push((process, Rc::clone(&alive)));
        alive
    }

    fn deliver_to_scope(&self, scope: &ScopeName, signal: KillSignal) {
        for process in self.shared.processes.borrow().iter() {
            if process.scope.as_deref() == Some(scope.as_str()) {
                let _ = process.to_script.send(Delivery::Scope(signal));
            }
        }
        for (process, alive) in self.shared.recovered.borrow().iter() {
            if &process.scope == scope && signal == KillSignal::Kill {
                alive.set(false);
            }
        }
    }
}

pub(crate) struct FakeChild {
    shared: Rc<ProcShared>,
    pipes: Option<Pipes>,
    exit: watch::Receiver<bool>,
}

impl Child for FakeChild {
    fn pid(&self) -> u32 {
        self.shared.pid
    }

    fn take_pipes(&mut self) -> io::Result<Pipes> {
        self.pipes
            .take()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))
    }

    async fn exited(&self) -> io::Result<()> {
        let mut exit = self.exit.clone();
        let _ = exit.wait_for(|exited| *exited).await;
        Ok(())
    }

    fn exit(&self) -> io::Result<Option<ExitInfo>> {
        Ok(self.shared.exit.get())
    }

    fn signal_group(&self, signal: GroupSignal) -> Result<(), SignalError> {
        self.shared.signals.borrow_mut().push(signal);
        match *self.shared.signal_error.borrow() {
            Some(SignalErrorKind::Gone) => return Err(SignalError::Gone),
            Some(SignalErrorKind::Failed) => {
                return Err(SignalError::Failed(io::Error::other("signal refused")));
            }
            None => {}
        }
        let _ = self.shared.to_script.send(Delivery::Group(signal));
        Ok(())
    }

    fn reap(self) -> io::Result<ExitInfo> {
        let info = self
            .shared
            .exit
            .get()
            .ok_or_else(|| io::Error::from(io::ErrorKind::WouldBlock))?;
        self.shared.reaped.set(true);
        Ok(info)
    }
}

fn exit_status(success: bool) -> ExitStatus {
    ExitStatus::from_raw(if success { 0 } else { 1 << 8 })
}

impl Os for Fake {
    type Child = FakeChild;

    fn launch(&self, command: &LaunchCommand) -> io::Result<FakeChild> {
        let index = self.shared.launches.borrow().len();
        self.shared.launches.borrow_mut().push(command.clone());
        let script = match (self.shared.launch_script.borrow_mut())(index, command) {
            Launch::Run(script) => script,
            Launch::Fail(error) => return Err(error),
        };
        let pid = self.shared.next_pid.get();
        self.shared.next_pid.set(pid + 1);
        let (stdout_writer, stdout_reader) = duplex(1 << 20);
        let (stderr_writer, stderr_reader) = duplex(1 << 20);
        let (to_script, signals) = unbounded_channel();
        let (exited, exit) = watch::channel(false);
        let shared = Rc::new(ProcShared {
            pid,
            scope: scope_in(command),
            ready: Cell::new(true),
            usage: RefCell::new(Ok(Usage {
                cpu_time: Duration::ZERO,
                cpu: 0,
            })),
            threads: RefCell::new(Ok(Threads::default())),
            signals: RefCell::default(),
            signal_error: RefCell::default(),
            reaped: Cell::new(false),
            exit: Cell::new(None),
            exited,
            to_script,
        });
        self.shared.processes.borrow_mut().push(Rc::clone(&shared));
        let ctx = ProcCtx {
            shared: Rc::clone(&shared),
            stdout: stdout_writer,
            stderr: stderr_writer,
            signals,
        };
        tokio::task::spawn_local(script(ctx));
        Ok(FakeChild {
            shared,
            pipes: Some(Pipes {
                stdout: Box::pin(stdout_reader),
                stderr: Box::pin(stderr_reader),
            }),
            exit,
        })
    }

    fn in_scope(&self, pid: u32, scope: &ScopeName) -> Result<bool, ProcError> {
        let processes = self.shared.processes.borrow();
        let process = processes.iter().find(|process| process.pid == pid);
        Ok(process.is_some_and(|process| {
            process.ready.get() && process.scope.as_deref() == Some(scope.as_str())
        }))
    }

    fn usage(&self, pid: u32) -> Result<Usage, ProcError> {
        let processes = self.shared.processes.borrow();
        processes
            .iter()
            .find(|process| process.pid == pid)
            .map_or(Err(ProcError::Gone), |process| process.usage.borrow().clone())
    }

    fn threads(&self, pid: u32) -> Result<Threads, ProcError> {
        let processes = self.shared.processes.borrow();
        processes
            .iter()
            .find(|process| process.pid == pid)
            .map_or(Err(ProcError::Gone), |process| clone_threads(&process.threads.borrow()))
    }

    async fn systemd(
        &self,
        operation: &Systemd,
        deadline: Instant,
    ) -> Result<togi_command::Output, togi_command::Error> {
        self.shared
            .systemd_calls
            .borrow_mut()
            .push((operation.clone(), Instant::now()));
        let scripted = (self.shared.systemd_script.borrow_mut())(operation);
        let reply = match scripted {
            Some(reply) => reply,
            None => {
                let mut text = String::new();
                match operation {
                    Systemd::Kill { scope, signal } => self.deliver_to_scope(scope, *signal),
                    Systemd::Stop { scope } => self.shared.units.borrow_mut().retain(|u| u != scope),
                    Systemd::ListScopes => {
                        for unit in self.shared.units.borrow().iter() {
                            text.push_str(&format!("{} loaded active running trial\n", unit.unit()));
                        }
                    }
                    Systemd::Probe { .. } => {}
                }
                Reply::Output {
                    success: true,
                    text,
                }
            }
        };
        match reply {
            Reply::Output { success, text } => Ok(togi_command::Output {
                status: exit_status(success),
                stdout: text.into_bytes(),
                stderr: Vec::new(),
            }),
            Reply::Hang => {
                sleep_until(deadline).await;
                Err(togi_command::Error::TimedOut {
                    program: "systemctl".into(),
                    timeout: Duration::ZERO,
                })
            }
            Reply::Fail(error) => Err(error),
        }
    }

    fn scope_processes(&self) -> Result<Vec<ScopeProcess>, ProcError> {
        Ok(self
            .shared
            .recovered
            .borrow()
            .iter()
            .filter(|(_, alive)| alive.get())
            .map(|(process, _)| process.clone())
            .collect())
    }

    fn process_alive(&self, process: &ScopeProcess) -> Result<bool, ProcError> {
        Ok(self
            .shared
            .recovered
            .borrow()
            .iter()
            .any(|(known, alive)| known == process && alive.get()))
    }
}

fn clone_threads(threads: &Result<Threads, ProcError>) -> Result<Threads, ProcError> {
    match threads {
        Ok(found) => Ok(Threads {
            threads: found.threads.clone(),
            errors: found.errors.iter().map(|e| ProcError::Observation(e.to_string())).collect(),
        }),
        Err(error) => Err(match error {
            ProcError::Gone => ProcError::Gone,
            ProcError::Observation(text) => ProcError::Observation(text.clone()),
        }),
    }
}
