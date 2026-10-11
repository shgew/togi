use std::path::PathBuf;
use std::sync::Arc;

use togi_backend::Backend;
use togi_machine::{
    CancellationToken, CoreInfo, Partial, PmTableReader, Running, Started, Stopped, Swept,
    TrialConditions, TrialResult, TrialSpec, Trials,
};

use crate::error::Error;
use crate::identity::BackendIdentity;
use crate::runtime::Runtime;
use crate::supervisor::{Event, Finished, Handle, TrialCommand};

/// The backend adapters by kind.
#[derive(Clone, Default)]
pub struct Backends {
    mprime: Option<Arc<dyn Backend>>,
    ycruncher: Option<Arc<dyn Backend>>,
}

impl Backends {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(mut self, backend: Arc<dyn Backend>) -> Self {
        match backend.kind() {
            togi_machine::Backend::Mprime => self.mprime = Some(backend),
            togi_machine::Backend::Ycruncher => self.ycruncher = Some(backend),
        }
        self
    }

    pub fn get(&self, kind: togi_machine::Backend) -> Option<&Arc<dyn Backend>> {
        match kind {
            togi_machine::Backend::Mprime => self.mprime.as_ref(),
            togi_machine::Backend::Ycruncher => self.ycruncher.as_ref(),
        }
    }
}

/// The production inputs of a runner.
pub struct Options {
    pub dir: PathBuf,
    pub backends: Backends,
    pub topology: Vec<CoreInfo>,
    pub identity: Option<BackendIdentity>,
    pub pm_table: Option<Arc<dyn PmTableReader + Send + Sync>>,
}

pub struct Runner {
    dir: PathBuf,
    runtime: Runtime,
}

impl Runner {
    pub fn new(options: Options) -> Result<Self, Error> {
        let _ = options;
        unimplemented!()
    }

    /// The preflight check: a scope pinned to CPU 0 is created and ends, within 30 s.
    pub fn probe_scope(&self) -> Result<String, Error> {
        unimplemented!()
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.runtime.shutdown();
    }
}

impl Trials for Runner {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, togi_machine::Error> {
        let _ = cancel;
        unimplemented!()
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let _ = (cancel, spec, ready);
        unimplemented!()
    }

    fn ended(&mut self, id: &str) -> Result<(), togi_machine::Error> {
        let _ = id;
        unimplemented!()
    }

    fn samples<'a>(&'a self, id: &'a str) -> Box<dyn Iterator<Item = TrialConditions> + 'a> {
        let _ = (id, &self.dir);
        unimplemented!()
    }
}

/// A started trial. Stopping it consumes it, so it can neither be used afterwards nor yield proof
/// without teardown.
///
/// ```compile_fail
/// fn proof(trial: Box<dyn togi_machine::Running>) -> togi_machine::Stopped {
///     togi_machine::Stopped::confirmed()
/// }
/// ```
pub struct Trial {
    started: Started,
    commands: tokio::sync::mpsc::UnboundedSender<TrialCommand>,
    events: std::sync::mpsc::Receiver<Event>,
    ended: Option<Finished>,
}

impl Running for Trial {
    fn started(&self) -> Started {
        self.started.clone()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn togi_machine::Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        let _ = (cancel, report, &self.commands, &self.events, &self.ended);
        unimplemented!()
    }

    fn stop(self: Box<Self>) -> Result<Stopped, togi_machine::Error> {
        unimplemented!()
    }
}

impl Drop for Trial {
    fn drop(&mut self) {
        let _ = self.commands.send(TrialCommand::Abandon);
    }
}

impl From<Handle> for Trial {
    fn from(handle: Handle) -> Self {
        Self {
            started: handle.started,
            commands: handle.commands,
            events: handle.events,
            ended: None,
        }
    }
}

pub(crate) fn _unused(_: CoreInfo) {}
