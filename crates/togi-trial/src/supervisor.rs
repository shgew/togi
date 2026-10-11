use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use tokio::sync::mpsc::UnboundedSender;
use togi_machine::{
    CancellationToken, CoreInfo, Partial, PmTableReader, Sample, Signal, CoreId, Started, Stopped,
    TrialResult, TrialSpec,
};

use crate::error::Error;
use crate::launch::Confinement;
use crate::os::Os;
use crate::runner::Backends;
use crate::sensors::Roots;
use crate::timing::Timing;

/// Everything a trial needs from the runner, owned by the runtime thread.
pub(crate) struct Env<O: Os> {
    pub(crate) os: Rc<O>,
    pub(crate) timing: Timing,
    pub(crate) roots: Roots,
    pub(crate) dir: PathBuf,
    pub(crate) backends: Backends,
    pub(crate) topology: Vec<CoreInfo>,
    pub(crate) confinement: Confinement,
    pub(crate) pm_table: Option<Arc<dyn PmTableReader + Send + Sync>>,
}

/// What the handle tells its supervisor.
#[derive(Debug)]
pub(crate) enum TrialCommand {
    /// A cancellation token fired.
    Cancel,
    Stop,
    /// The handle was dropped before a stop.
    Abandon,
}

/// What the supervisor tells the thread blocked in `wait`.
#[derive(Debug)]
pub(crate) enum Event {
    Progress(String),
    Sample(Sample),
    Signal {
        core: CoreId,
        signal: Signal,
        detail: String,
    },
    Finished(Box<Finished>),
}

/// The end of supervision and of teardown, sent exactly once.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(crate) result: TrialResult,
    /// A runner error that is not containment: cancellation, a failed load step, storage.
    pub(crate) error: Option<Error>,
    pub(crate) teardown: Result<Stopped, crate::error::Containment>,
}

/// The `Trial` side of one supervisor task.
pub(crate) struct Handle {
    pub(crate) commands: UnboundedSender<TrialCommand>,
    pub(crate) events: Receiver<Event>,
    pub(crate) started: Started,
}

/// Prepares, launches and begins supervising a trial. Timing starts when this returns.
/// On failure the same teardown ran; `Some` in the partial value proves the rollback confirmed.
pub(crate) async fn start<O: Os>(
    env: Rc<Env<O>>,
    cancel: CancellationToken,
    spec: TrialSpec,
    ready: Stopped,
) -> Result<Handle, Partial<Option<Stopped>>> {
    let _ = (env, cancel, spec, ready);
    unimplemented!()
}
