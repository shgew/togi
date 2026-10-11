use std::rc::Rc;
use std::sync::mpsc::{Sender, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use togi_machine::{CancellationToken, Partial, Stopped, Swept, TrialSpec};

use crate::error::Error;
use crate::os::Os;
use crate::supervisor::{Env, Handle};

pub(crate) type Reply<T> = SyncSender<T>;

pub(crate) enum Command {
    Start {
        cancel: CancellationToken,
        spec: Box<TrialSpec>,
        ready: Stopped,
        reply: Reply<Result<Handle, Partial<Option<Stopped>>>>,
    },
    Sweep {
        reply: Reply<Result<Swept, Error>>,
    },
    Probe {
        reply: Reply<Result<String, Error>>,
    },
    Shutdown,
}

/// The one thread that runs the runtime and launches every process.
pub(crate) struct Runtime {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    thread: Option<JoinHandle<()>>,
    finished: std::sync::mpsc::Receiver<()>,
}

pub(crate) const JOIN_BOUND: Duration = Duration::from_secs(1);

impl Runtime {
    /// Builds the `current_thread` runtime and `LocalSet` on a new thread; `make_env` runs there.
    pub(crate) fn spawn<O: Os>(
        make_env: impl FnOnce() -> Env<O> + Send + 'static,
    ) -> Result<Self, Error> {
        let _ = make_env;
        unimplemented!()
    }

    pub(crate) fn send(&self, command: Command) -> Result<(), Error> {
        self.commands.send(command).map_err(|_| Error::RuntimeGone)
    }

    /// Asks the thread to end and waits at most `JOIN_BOUND`; stuck work is abandoned.
    pub(crate) fn shutdown(&mut self) {
        let _ = (&self.thread, &self.finished);
        unimplemented!()
    }
}

/// Refuses to block inside an async runtime.
pub(crate) fn outside_runtime(operation: &'static str) -> Result<(), Error> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(Error::InsideRuntime { operation });
    }
    Ok(())
}

pub(crate) fn _unused(_: Rc<()>, _: Sender<()>) {}
