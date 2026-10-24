use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender, channel, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use togi_machine::{CancellationToken, Partial, Stopped, Swept, TrialSpec};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::LocalSet;

use crate::clock::Clock;
use crate::error::Error;
use crate::os::Os;
use crate::supervisor::{Env, Handle, TrialCommand, start};
use crate::sweep;
use crate::timing::Timing;

pub(crate) type Reply<T> = SyncSender<T>;

pub(crate) enum Command {
    /// `cancel` is this start's own token, a child of the caller's: the synchronous side fires
    /// it when it gives up waiting and the runtime when it shuts down.
    Start {
        cancel: CancellationToken,
        spec: Box<TrialSpec>,
        ready: Stopped,
        reply: Reply<Result<Delivered, Partial<Option<Stopped>>>>,
    },
    /// `cancel` is this sweep's own token, as for `Start`.
    Sweep {
        cancel: CancellationToken,
        reply: Reply<Result<Swept, Error>>,
    },
    Probe {
        reply: Reply<Result<String, Error>>,
    },
    Shutdown,
}

/// The one thread that runs the runtime and launches every process.
pub(crate) struct Runtime {
    commands: UnboundedSender<Command>,
    thread: Option<JoinHandle<()>>,
    finished: Receiver<()>,
    join_bound: Duration,
    clock: Clock,
}

/// What the synchronous side waits beyond the supervisors' own shutdown slot.
const JOIN_SLACK: Duration = Duration::from_secs(1);

impl Runtime {
    /// Builds the `current_thread` runtime and `LocalSet` on a new thread; `make_env` runs there,
    /// so nothing the environment holds has to be `Send`.
    pub(crate) fn spawn<O: Os>(
        make_env: impl FnOnce() -> Env<O> + Send + 'static,
        clock: Clock,
    ) -> Result<Self, Error> {
        Self::spawn_with(make_env, true, clock)
    }

    /// Runs in-memory adapters without constructing the operating system's I/O reactor.
    pub(crate) fn spawn_timer_only<O: Os>(
        make_env: impl FnOnce() -> Env<O> + Send + 'static,
        clock: Clock,
    ) -> Result<Self, Error> {
        Self::spawn_with(make_env, false, clock)
    }

    fn spawn_with<O: Os>(
        make_env: impl FnOnce() -> Env<O> + Send + 'static,
        io: bool,
        clock: Clock,
    ) -> Result<Self, Error> {
        let join_bound = Timing::default().stop_bound() + JOIN_SLACK;
        let (bound_tx, bound_rx) = sync_channel::<Duration>(1);
        let (commands, mut inbox) = unbounded_channel::<Command>();
        let (finished_tx, finished) = channel();
        let (ready_tx, ready_rx) = sync_channel::<Result<(), Error>>(1);
        #[cfg(test)]
        let paused = clock.paused();
        let thread = std::thread::Builder::new()
            .name("togi-trial".to_owned())
            .spawn(move || {
                let mut builder = tokio::runtime::Builder::new_current_thread();
                builder.enable_time();
                if io {
                    builder.enable_io();
                }
                #[cfg(test)]
                builder.start_paused(paused);
                let runtime = match builder.build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(Error::RuntimeBuild(error)));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                let env = Rc::new(make_env());
                let _ = bound_tx.send(env.timing.stop_bound() + JOIN_SLACK);
                LocalSet::new().block_on(&runtime, async move {
                    let mut tasks: Vec<Task> = Vec::new();
                    while let Some(command) = inbox.recv().await {
                        match command {
                            Command::Shutdown => break,
                            command @ (Command::Start { .. }
                            | Command::Sweep { .. }
                            | Command::Probe { .. }) => {
                                tasks.retain(|task| !task.handle.is_finished());
                                tasks.push(Task::spawn(&env, command));
                            }
                        }
                    }
                    contain_all(&env, tasks).await;
                });
                runtime.shutdown_background();
                let _ = finished_tx.send(());
            })
            .map_err(Error::RuntimeThread)?;
        ready_rx.recv().map_err(|_| Error::RuntimeGone)??;
        let join_bound = bound_rx.recv().unwrap_or(join_bound);
        Ok(Self {
            commands,
            thread: Some(thread),
            finished,
            join_bound,
            clock,
        })
    }

    pub(crate) fn send(&self, command: Command) -> Result<(), Error> {
        self.commands.send(command).map_err(|_| Error::RuntimeGone)
    }

    /// Asks the thread to tear down every live trial and end, and waits at most for the
    /// supervisors' shutdown slot plus a second; a thread stuck past that is abandoned.
    pub(crate) fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if self
            .clock
            .recv_timeout(&self.finished, self.join_bound)
            .is_ok()
        {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

/// A started trial on its way to the caller. If it is dropped undelivered or unclaimed, because
/// the caller stopped waiting for the start, the trial is abandoned so its supervisor tears it
/// down; the caller claims it with `claim`.
pub(crate) struct Delivered {
    handle: Handle,
    guard: Abandon,
}

struct Abandon {
    commands: UnboundedSender<TrialCommand>,
    armed: bool,
}

impl Delivered {
    fn new(handle: Handle) -> Self {
        let guard = Abandon {
            commands: handle.commands.clone(),
            armed: true,
        };
        Self { handle, guard }
    }

    pub(crate) fn claim(self) -> Handle {
        let Self { handle, mut guard } = self;
        guard.armed = false;
        handle
    }
}

impl Drop for Abandon {
    fn drop(&mut self) {
        if self.armed && self.commands.send(TrialCommand::Abandon).is_ok() {
            report("a trial started after its caller gave up waiting was abandoned");
        }
    }
}

async fn serve<O: Os>(env: Rc<Env<O>>, command: Command) {
    match command {
        Command::Start {
            cancel,
            spec,
            ready,
            reply,
        } => {
            let outcome = start(env, cancel, *spec, ready).await.map(Delivered::new);
            if let Err(std::sync::mpsc::SendError(Err(Partial { value: None, error }))) =
                reply.send(outcome)
            {
                report(&format!(
                    "a start nobody waits for ended without confirmed cleanup: {}",
                    togi_machine::error_chain(&error)
                ));
            }
        }
        Command::Sweep { cancel, reply } => {
            let outcome = async {
                let (mut detail, stopped) = sweep::sweep(&env, &cancel).await?;
                let mut reports = Vec::new();
                env.frequency
                    .restore(&CancellationToken::new(), &mut reports)
                    .await
                    .map_err(Error::Frequency)?;
                for line in reports {
                    detail.push_str("; ");
                    detail.push_str(&line);
                }
                // A sweep proves nothing runs, so threads still confined by a start that never
                // reached its teardown can have their own CPUs back.
                if let Some(moved) = env.controller.restore() {
                    detail.push_str("; ");
                    detail.push_str(&crate::supervisor::restored_line(moved));
                }
                Ok::<_, Error>(Swept { detail, stopped })
            }
            .await;
            if let Err(std::sync::mpsc::SendError(Err(error))) = reply.send(outcome) {
                report(&format!(
                    "a sweep nobody waits for failed: {}",
                    togi_machine::error_chain(&error)
                ));
            }
        }
        Command::Probe { reply } => {
            let _ = reply.send(sweep::probe(&env).await);
        }
        Command::Shutdown => {}
    }
}

/// What the runtime thread runs for a command, kept so shutdown can cancel and await it.
struct Task {
    what: &'static str,
    cancel: Option<CancellationToken>,
    handle: tokio::task::JoinHandle<()>,
}

impl Task {
    fn spawn<O: Os>(env: &Rc<Env<O>>, command: Command) -> Self {
        let (what, cancel) = match &command {
            Command::Start { cancel, .. } => ("start", Some(cancel.clone())),
            Command::Sweep { cancel, .. } => ("sweep", Some(cancel.clone())),
            Command::Probe { .. } | Command::Shutdown => ("probe", None),
        };
        Self {
            what,
            cancel,
            handle: tokio::task::spawn_local(serve(Rc::clone(env), command)),
        }
    }
}

/// Ends everything in flight within one shared slot, so dropping the runtime never leaves a
/// workload running. Every live supervisor is asked to tear down at once, and the registry
/// abandons any a pending start adds later, so rollbacks and teardowns overlap in the slot.
/// Whatever is still running at the end of the slot is reported.
async fn contain_all<O: Os>(env: &Env<O>, tasks: Vec<Task>) {
    let deadline = tokio::time::Instant::now() + env.timing.stop_bound();
    for task in &tasks {
        if let Some(cancel) = &task.cancel {
            cancel.cancel();
        }
    }
    let mut live = env.supervisors.borrow_mut().close();
    for task in tasks {
        if tokio::time::timeout_at(deadline, task.handle)
            .await
            .is_err()
        {
            report(&format!(
                "a {} was still running when its runner shut down",
                task.what
            ));
        }
    }
    live.extend(env.supervisors.borrow_mut().close());
    for supervised in live {
        if tokio::time::timeout_at(deadline, supervised.task)
            .await
            .is_err()
        {
            report(&format!(
                "trial {} was still not torn down when its runner shut down",
                supervised.trial
            ));
        }
    }
}

#[cfg(test)]
pub(crate) async fn contain_pending<O: Os>(
    env: &Env<O>,
    handles: Vec<tokio::task::JoinHandle<()>>,
) {
    let tasks = handles
        .into_iter()
        .map(|handle| Task {
            what: "start",
            cancel: None,
            handle,
        })
        .collect();
    contain_all(env, tasks).await;
}

#[cfg(test)]
pub(crate) async fn serve_detached<O: Os>(env: Rc<Env<O>>, command: Command) {
    serve(env, command).await;
}

#[cfg(test)]
thread_local! {
    /// What `report` would have written to stderr.
    pub(crate) static RUNTIME_REPORTS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(not(test))]
#[expect(
    clippy::print_stderr,
    reason = "the runner is being dropped or its caller gave up, so no caller is left to report to"
)]
fn report(message: &str) {
    eprintln!("togi: {message}");
}

#[cfg(test)]
fn report(message: &str) {
    RUNTIME_REPORTS.with_borrow_mut(|reports| reports.push(format!("togi: {message}")));
}

/// Refuses to block inside an async runtime.
pub(crate) fn outside_runtime(operation: &'static str) -> Result<(), Error> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(Error::InsideRuntime { operation });
    }
    Ok(())
}
