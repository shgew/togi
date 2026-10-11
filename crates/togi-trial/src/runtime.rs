use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, channel, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::LocalSet;
use togi_machine::{CancellationToken, Partial, Stopped, Swept, TrialSpec};

use crate::error::Error;
use crate::os::Os;
use crate::supervisor::{Env, Handle, start};
use crate::sweep;

pub(crate) type Reply<T> = SyncSender<T>;

pub(crate) enum Command {
    Start {
        cancel: CancellationToken,
        spec: Box<TrialSpec>,
        ready: Stopped,
        reply: Reply<Result<Handle, Partial<Option<Stopped>>>>,
    },
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
}

pub(crate) const JOIN_BOUND: Duration = Duration::from_secs(1);

impl Runtime {
    /// Builds the `current_thread` runtime and `LocalSet` on a new thread; `make_env` runs there,
    /// so nothing the environment holds has to be `Send`.
    pub(crate) fn spawn<O: Os>(
        make_env: impl FnOnce() -> Env<O> + Send + 'static,
    ) -> Result<Self, Error> {
        let (commands, mut inbox) = unbounded_channel::<Command>();
        let (finished_tx, finished) = channel();
        let (ready_tx, ready_rx) = sync_channel::<Result<(), Error>>(1);
        let thread = std::thread::Builder::new()
            .name("togi-trial".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(Error::RuntimeBuild(error)));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                let env = Rc::new(make_env());
                LocalSet::new().block_on(&runtime, async move {
                    while let Some(command) = inbox.recv().await {
                        match command {
                            Command::Shutdown => break,
                            command => {
                                tokio::task::spawn_local(serve(Rc::clone(&env), command));
                            }
                        }
                    }
                });
                let _ = finished_tx.send(());
            })
            .map_err(Error::RuntimeThread)?;
        ready_rx.recv().map_err(|_| Error::RuntimeGone)??;
        Ok(Self {
            commands,
            thread: Some(thread),
            finished,
        })
    }

    pub(crate) fn send(&self, command: Command) -> Result<(), Error> {
        self.commands.send(command).map_err(|_| Error::RuntimeGone)
    }

    /// Asks the thread to end and waits at most `JOIN_BOUND`; stuck work is abandoned, and the
    /// launchers' parent-death signal ends whatever still runs when the thread does.
    pub(crate) fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if self.finished.recv_timeout(JOIN_BOUND).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
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
            let _ = reply.send(start(env, cancel, *spec, ready).await);
        }
        Command::Sweep { cancel, reply } => {
            let woken = Arc::new(Notify::new());
            let _registration = {
                let woken = Arc::clone(&woken);
                cancel.on_cancel(move || woken.notify_one())
            };
            let outcome = tokio::select! {
                outcome = sweep::sweep(&env) => outcome.map(|(detail, stopped)| Swept { detail, stopped }),
                () = woken.notified() => Err(Error::Cancelled),
            };
            let _ = reply.send(outcome);
        }
        Command::Probe { reply } => {
            let _ = reply.send(sweep::probe(&env).await);
        }
        Command::Shutdown => {}
    }
}

/// Refuses to block inside an async runtime.
pub(crate) fn outside_runtime(operation: &'static str) -> Result<(), Error> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(Error::InsideRuntime { operation });
    }
    Ok(())
}
