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
use std::path::Path;
use std::pin::Pin;
use std::process::ExitStatus;
use std::rc::Rc;
use std::sync::Arc;
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
    /// Never answers and ignores its deadline: only a caller's own timeout ends the wait.
    Stall,
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
    /// Armed one-shot failures, shared by every process of the fake: the first delivery of a
    /// matching signal fails and disarms its entry.
    fail_once: Rc<RefCell<Vec<(GroupSignal, SignalErrorKind)>>>,
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

/// How a `/proc` scan of the trial's scopes goes wrong.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScanFault {
    /// The scan fails at once.
    Fail,
    /// The scan finds nothing, but only after its deadline has passed.
    Late,
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

/// What a process script holds. `account` is the `--uid`/`--gid` its scope runs it under, if any.
pub(crate) struct ProcCtx {
    pub(crate) shared: Rc<ProcShared>,
    pub(crate) stdout: DuplexStream,
    pub(crate) stderr: DuplexStream,
    pub(crate) signals: UnboundedReceiver<Delivery>,
    pub(crate) account: Option<(u32, u32)>,
    pub(crate) memory: crate::watch::Memory,
    disk_writes: bool,
}

impl ProcCtx {
    pub(crate) async fn print(&mut self, text: &str) {
        let _ = self.stdout.write_all(text.as_bytes()).await;
    }

    pub(crate) async fn eprint(&mut self, text: &str) {
        let _ = self.stderr.write_all(text.as_bytes()).await;
    }

    /// Writes through the shared memory adapter, or the explicitly selected kernel-test adapter.
    pub(crate) fn write_file(
        &self,
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> io::Result<()> {
        if !self.disk_writes {
            return self.memory.write_file(
                path,
                contents,
                Some(self.account.map_or(65534, |(uid, _)| uid)),
            );
        }
        let mut file = std::fs::File::create(path)?;
        if let Some((uid, gid)) = self.account {
            std::os::unix::fs::fchown(&file, Some(uid), Some(gid))?;
        }
        io::Write::write_all(&mut file, contents.as_ref())
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
                    Delivery::Group(_) | Delivery::Scope(_) => ExitInfo::Signal(9),
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

type ThreadsHook = Box<dyn FnMut(u32)>;
type ScanHook = Box<dyn FnMut()>;

struct Shared {
    launches: RefCell<Vec<LaunchCommand>>,
    processes: RefCell<Vec<Rc<ProcShared>>>,
    systemd_calls: RefCell<Vec<(Systemd, Instant)>>,
    launch_script: RefCell<LaunchScript>,
    systemd_script: RefCell<SystemdScript>,
    threads_hook: RefCell<Option<ThreadsHook>>,
    scan_hook: RefCell<Option<ScanHook>>,
    units: RefCell<Vec<String>>,
    ready_default: Cell<bool>,
    fail_pipes: Cell<bool>,
    fail_alive: Cell<bool>,
    hang_scans: Cell<bool>,
    scan_fault: Cell<Option<ScanFault>>,
    signal_error_default: Cell<Option<SignalErrorKind>>,
    fail_once: Rc<RefCell<Vec<(GroupSignal, SignalErrorKind)>>>,
    recovered: RefCell<Vec<(ScopeProcess, Recovered)>>,
    next_pid: Cell<u32>,
}

/// The fake OS; clones share one state, so a test keeps a handle after giving one to a runner.
#[derive(Clone)]
pub(crate) struct Fake {
    shared: Rc<Shared>,
    memory: crate::watch::Memory,
    disk_writes: bool,
}

impl Default for Fake {
    fn default() -> Self {
        Self::with_memory(crate::watch::Memory::new(Arc::new(
            togi_journal::fs::MemFs::new(),
        )))
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

fn flag_in(command: &LaunchCommand, flag: &str) -> Option<String> {
    let position = command.argv.iter().position(|arg| arg == flag)?;
    Some(
        command
            .argv
            .get(position + 1)?
            .to_string_lossy()
            .into_owned(),
    )
}

fn scope_in(command: &LaunchCommand) -> Option<String> {
    flag_in(command, "--unit")
}

fn account_in(command: &LaunchCommand) -> Option<(u32, u32)> {
    Some((
        flag_in(command, "--uid")?.parse().ok()?,
        flag_in(command, "--gid")?.parse().ok()?,
    ))
}

impl Fake {
    pub(crate) fn with_memory(memory: crate::watch::Memory) -> Self {
        Self {
            memory,
            disk_writes: false,
            shared: Rc::new(Shared {
                launches: RefCell::default(),
                processes: RefCell::default(),
                systemd_calls: RefCell::default(),
                launch_script: RefCell::new(Box::new(|_, _| {
                    Launch::Run(script(|mut ctx| async move { ctx.until_killed().await }))
                })),
                systemd_script: RefCell::new(Box::new(|_| None)),
                threads_hook: RefCell::new(None),
                scan_hook: RefCell::new(None),
                units: RefCell::default(),
                ready_default: Cell::new(true),
                fail_pipes: Cell::new(false),
                fail_alive: Cell::new(false),
                hang_scans: Cell::new(false),
                scan_fault: Cell::new(None),
                signal_error_default: Cell::new(None),
                fail_once: Rc::default(),
                recovered: RefCell::default(),
                next_pid: Cell::new(1000),
            }),
        }
    }

    /// Only kernel ownership tests select real backend file writes.
    pub(crate) fn disk() -> Self {
        Self {
            disk_writes: true,
            ..Self::default()
        }
    }

    pub(crate) fn append_file(
        &self,
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
    ) -> io::Result<()> {
        self.memory.append_file(path, contents, Some(65534))
    }

    pub(crate) fn on_launch(&self, script: impl FnMut(usize, &LaunchCommand) -> Launch + 'static) {
        *self.shared.launch_script.borrow_mut() = Box::new(script);
    }

    /// Answers systemd calls; `None` falls back to the default (success, effect applied).
    pub(crate) fn on_systemd(&self, script: impl FnMut(&Systemd) -> Option<Reply> + 'static) {
        *self.shared.systemd_script.borrow_mut() = Box::new(script);
    }

    /// Runs `hook(pid)` at the start of every thread read, before the process is looked up and
    /// with no process state borrowed, so it may change a process or burn real time.
    pub(crate) fn on_threads(&self, hook: impl FnMut(u32) + 'static) {
        *self.shared.threads_hook.borrow_mut() = Some(Box::new(hook));
    }

    /// Every signal to a new process fails like this.
    pub(crate) fn set_signal_error_default(&self, kind: Option<SignalErrorKind>) {
        self.shared.signal_error_default.set(kind);
    }

    /// The next delivery of `signal` to any process fails like `kind`, once: a refused `SIGSTOP`
    /// followed by a teardown whose signals succeed.
    pub(crate) fn fail_signal_once(&self, signal: GroupSignal, kind: SignalErrorKind) {
        self.shared.fail_once.borrow_mut().push((signal, kind));
    }

    /// Checking whether a captured process lives fails.
    pub(crate) fn fail_alive(&self) {
        self.shared.fail_alive.set(true);
    }

    /// Every `/proc` scan hangs until its deadline.
    pub(crate) fn hang_scans(&self) {
        self.shared.hang_scans.set(true);
    }

    /// Scans of the trial's scopes fail like `fault`, while survivor checks stay normal.
    pub(crate) fn fault_scans(&self, fault: ScanFault) {
        self.shared.scan_fault.set(Some(fault));
    }

    /// Runs `hook` at the start of every scan of the trial's scopes, with no process state
    /// borrowed, so it can act as a writer still running when the scan began.
    pub(crate) fn on_scan(&self, hook: impl FnMut() + 'static) {
        *self.shared.scan_hook.borrow_mut() = Some(Box::new(hook));
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
        let once = {
            let mut armed = self.shared.fail_once.borrow_mut();
            armed
                .iter()
                .position(|(armed_signal, _)| *armed_signal == signal)
                .map(|position| armed.remove(position).1)
        };
        match once.or(*self.shared.signal_error.borrow()) {
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
            fail_once: Rc::clone(&self.shared.fail_once),
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
            account: account_in(command),
            memory: self.memory.clone(),
            disk_writes: self.disk_writes,
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
        if let Some(hook) = self.shared.threads_hook.borrow_mut().as_mut() {
            hook(pid);
        }
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
        // Like `togi_command::run`: a deadline that is not in the future times out without
        // starting the command, so no script runs and no effect applies.
        if deadline <= Instant::now() {
            return Err(togi_command::Error::TimedOut {
                program: "systemctl".into(),
                timeout: Duration::ZERO,
            });
        }
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
            Reply::Stall => std::future::pending().await,
            Reply::Fail(error) => Err(error),
        }
    }

    async fn scope_processes(&self, deadline: Instant) -> Result<Vec<ScopeProcess>, ProcError> {
        if let Some(hook) = self.shared.scan_hook.borrow_mut().as_mut() {
            hook();
        }
        match self.shared.scan_fault.get() {
            Some(ScanFault::Fail) => {
                return Err(ProcError::Observation("scan failed".to_owned()));
            }
            Some(ScanFault::Late) => {
                sleep_until(deadline + Duration::from_millis(1)).await;
                return Ok(Vec::new());
            }
            None => {}
        }
        if self.shared.hang_scans.get() {
            sleep_until(deadline).await;
            return Err(ProcError::Deadline);
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
            return Err(ProcError::Deadline);
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
        Err(error) => Err(error.clone()),
    }
}
