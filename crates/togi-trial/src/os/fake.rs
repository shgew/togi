//! The scoped fake: readiness follows scripted cgroup membership, identity is valid, and every
//! systemd call is recorded and answered from a script. Time is tokio's, so tests pause it.
//!
//! A launched process is a script: an async block holding the pipe writers and receiving every
//! signal, free to print, sleep, ignore `SIGTERM` or hold the pipes after its leader exited.

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::future::Future;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::pin::Pin;
use std::process::ExitStatus;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWriteExt as _, DuplexStream, ReadBuf, duplex};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::watch;
use tokio::time::{Instant, sleep_until};

use super::{
    Child, ExitInfo, GroupSignal, KillSignal, LaunchFailure, Os, Pipes, SignalError, Systemd,
};
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
    /// The launcher runs `ProcessScript`, but setting up its exit watch failed afterwards.
    FailRetained(ProcessScript, io::Error),
}

/// What the fake systemd answers to one call.
pub(crate) enum Reply {
    Output {
        success: bool,
        text: String,
    },
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
    pub(crate) signal_times: RefCell<Vec<(Instant, GroupSignal)>>,
    stdout_failure: Rc<RefCell<PipeFailure>>,
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

#[derive(Default)]
struct PipeFailure {
    fail: bool,
    waker: Option<Waker>,
}

/// The read end of stdout; it can be made to fail like a pipe with an I/O error.
struct Flaky {
    inner: DuplexStream,
    failure: Rc<RefCell<PipeFailure>>,
}

impl AsyncRead for Flaky {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        {
            let mut failure = this.failure.borrow_mut();
            if failure.fail {
                return Poll::Ready(Err(io::Error::from_raw_os_error(5)));
            }
            failure.waker = Some(cx.waker().clone());
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl ProcShared {
    /// The next read of stdout fails with `EIO`.
    pub(crate) fn fail_stdout(&self) {
        let mut failure = self.stdout_failure.borrow_mut();
        failure.fail = true;
        if let Some(waker) = failure.waker.take() {
            waker.wake();
        }
    }

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
            matches!(
                delivery,
                Delivery::Group(GroupSignal::Kill) | Delivery::Scope(_)
            )
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

/// A process found by discovery: `alive` says whether it lives, `visible` whether it is still in
/// its scope, out of reach of a scope kill when false.
#[derive(Clone)]
pub(crate) struct Recovered {
    pub(crate) alive: Rc<Cell<bool>>,
    pub(crate) visible: Rc<Cell<bool>>,
}

type LaunchScript = Box<dyn FnMut(usize, &LaunchCommand) -> Launch>;
type SystemdScript = Box<dyn FnMut(&Systemd) -> Option<Reply>>;

struct Shared {
    launches: RefCell<Vec<LaunchCommand>>,
    processes: RefCell<Vec<Rc<ProcShared>>>,
    systemd_calls: RefCell<Vec<(Systemd, Instant)>>,
    launch_script: RefCell<LaunchScript>,
    systemd_script: RefCell<SystemdScript>,
    units: RefCell<Vec<String>>,
    ready_default: Cell<bool>,
    fail_pipes: Cell<bool>,
    fail_alive: Cell<bool>,
    hang_scans: Cell<bool>,
    signal_error_default: Cell<Option<SignalErrorKind>>,
    recovered: RefCell<Vec<(ScopeProcess, Recovered)>>,
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
                ready_default: Cell::new(true),
                fail_pipes: Cell::new(false),
                fail_alive: Cell::new(false),
                hang_scans: Cell::new(false),
                signal_error_default: Cell::new(None),
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
    Some(
        command
            .argv
            .get(position + 1)?
            .to_string_lossy()
            .into_owned(),
    )
}

impl Fake {
    pub(crate) fn on_launch(&self, script: impl FnMut(usize, &LaunchCommand) -> Launch + 'static) {
        *self.shared.launch_script.borrow_mut() = Box::new(script);
    }

    /// Answers systemd calls; `None` falls back to the default (success, effect applied).
    pub(crate) fn on_systemd(&self, script: impl FnMut(&Systemd) -> Option<Reply> + 'static) {
        *self.shared.systemd_script.borrow_mut() = Box::new(script);
    }

    /// Every signal to a new process fails like this.
    pub(crate) fn set_signal_error_default(&self, kind: Option<SignalErrorKind>) {
        self.shared.signal_error_default.set(kind);
    }

    /// Checking whether a captured process lives fails.
    pub(crate) fn fail_alive(&self) {
        self.shared.fail_alive.set(true);
    }

    /// Every `/proc` scan hangs until its deadline.
    pub(crate) fn hang_scans(&self) {
        self.shared.hang_scans.set(true);
    }

    /// Taking the pipes of new processes fails, as when registering them with the reactor fails.
    pub(crate) fn fail_pipes(&self) {
        self.shared.fail_pipes.set(true);
    }

    /// Whether new processes start inside their scope.
    pub(crate) fn set_ready_default(&self, ready: bool) {
        self.shared.ready_default.set(ready);
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

    /// A unit as `systemctl list-units` prints its name, valid or not.
    pub(crate) fn add_unit(&self, unit: &str) {
        self.shared.units.borrow_mut().push(unit.to_owned());
    }

    pub(crate) fn units(&self) -> Vec<String> {
        self.shared.units.borrow().clone()
    }

    /// A process found in a scope by `/proc` discovery.
    pub(crate) fn add_recovered(&self, process: ScopeProcess) -> Recovered {
        let recovered = Recovered {
            alive: Rc::new(Cell::new(true)),
            visible: Rc::new(Cell::new(true)),
        };
        self.shared
            .recovered
            .borrow_mut()
            .push((process, recovered.clone()));
        recovered
    }

    fn deliver_to_scope(&self, scope: &ScopeName, signal: KillSignal) {
        for process in self.shared.processes.borrow().iter() {
            if process.scope.as_deref() == Some(scope.as_str()) {
                let _ = process.to_script.send(Delivery::Scope(signal));
            }
        }
        for (process, recovered) in self.shared.recovered.borrow().iter() {
            if &process.scope == scope && signal == KillSignal::Kill && recovered.visible.get() {
                recovered.alive.set(false);
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
        self.shared
            .signal_times
            .borrow_mut()
            .push((Instant::now(), signal));
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

    fn launch(&self, command: &LaunchCommand) -> Result<FakeChild, LaunchFailure<FakeChild>> {
        let index = self.shared.launches.borrow().len();
        self.shared.launches.borrow_mut().push(command.clone());
        let (script, failure) = match (self.shared.launch_script.borrow_mut())(index, command) {
            Launch::Run(script) => (script, None),
            Launch::Fail(error) => return Err(error.into()),
            Launch::FailRetained(script, error) => (script, Some(error)),
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
            ready: Cell::new(self.shared.ready_default.get()),
            usage: RefCell::new(Ok(Usage {
                cpu_time: Duration::ZERO,
                cpu: 0,
            })),
            threads: RefCell::new(Ok(Threads::default())),
            signals: RefCell::default(),
            signal_times: RefCell::default(),
            stdout_failure: Rc::new(RefCell::new(PipeFailure::default())),
            signal_error: RefCell::new(self.shared.signal_error_default.get()),
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
        let flaky = Rc::clone(&shared.stdout_failure);
        let pipes = (!self.shared.fail_pipes.get()).then(|| Pipes {
            stdout: Box::pin(Flaky {
                inner: stdout_reader,
                failure: flaky,
            }),
            stderr: Box::pin(stderr_reader),
        });
        let child = FakeChild {
            shared,
            pipes,
            exit,
        };
        match failure {
            None => Ok(child),
            Some(source) => Err(LaunchFailure {
                child: Some(child),
                source,
            }),
        }
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
            .map_or(Err(ProcError::Gone), |process| {
                process.usage.borrow().clone()
            })
    }

    fn threads(&self, pid: u32) -> Result<Threads, ProcError> {
        let processes = self.shared.processes.borrow();
        processes
            .iter()
            .find(|process| process.pid == pid)
            .map_or(Err(ProcError::Gone), |process| {
                clone_threads(&process.threads.borrow())
            })
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
        let reply = if let Some(reply) = scripted {
            reply
        } else {
            let mut text = String::new();
            match operation {
                Systemd::Kill { scope, signal } => self.deliver_to_scope(scope, *signal),
                Systemd::Stop { scope } => {
                    self.shared
                        .units
                        .borrow_mut()
                        .retain(|unit| *unit != scope.unit());
                }
                Systemd::ListScopes => {
                    for unit in self.shared.units.borrow().iter() {
                        let _ = writeln!(text, "{unit} loaded active running trial");
                    }
                }
                Systemd::Probe { .. } => {}
            }
            Reply::Output {
                success: true,
                text,
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

    async fn scope_processes(&self, deadline: Instant) -> Result<Vec<ScopeProcess>, ProcError> {
        if self.shared.hang_scans.get() {
            sleep_until(deadline).await;
            return Err(ProcError::Observation("scan timed out".to_owned()));
        }
        let mut found: Vec<ScopeProcess> = self
            .shared
            .recovered
            .borrow()
            .iter()
            .filter(|(_, recovered)| recovered.alive.get() && recovered.visible.get())
            .map(|(process, _)| process.clone())
            .collect();
        for process in self.shared.processes.borrow().iter() {
            let scope = process
                .scope
                .as_deref()
                .map(|scope| format!("{scope}.scope"));
            if let Some(scope) = scope.as_deref().and_then(ScopeName::from_unit) {
                if !process.exited() {
                    found.push(ScopeProcess {
                        scope,
                        pid: process.pid,
                        starttime: 0,
                    });
                }
            }
        }
        Ok(found)
    }

    async fn survivors(
        &self,
        captured: &[ScopeProcess],
        deadline: Instant,
    ) -> Result<usize, ProcError> {
        if self.shared.fail_alive.get() {
            return Err(ProcError::Observation("permission denied".to_owned()));
        }
        if self.shared.hang_scans.get() {
            sleep_until(deadline).await;
            return Err(ProcError::Observation("scan timed out".to_owned()));
        }
        let recovered = self.shared.recovered.borrow();
        Ok(captured
            .iter()
            .filter(|process| {
                recovered
                    .iter()
                    .any(|(known, state)| known == *process && state.alive.get())
            })
            .count())
    }
}

fn clone_threads(threads: &Result<Threads, ProcError>) -> Result<Threads, ProcError> {
    match threads {
        Ok(found) => Ok(Threads {
            threads: found.threads.clone(),
            errors: found
                .errors
                .iter()
                .map(|e| ProcError::Observation(e.to_string()))
                .collect(),
        }),
        Err(error) => Err(match error {
            ProcError::Gone => ProcError::Gone,
            ProcError::Observation(text) => ProcError::Observation(text.clone()),
        }),
    }
}
