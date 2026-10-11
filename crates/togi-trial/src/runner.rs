use std::path::PathBuf;
use std::sync::Arc;

use togi_backend::Backend;
use togi_machine::{CoreInfo, PmTableReader};

use crate::identity::BackendIdentity;

#[cfg(target_os = "linux")]
pub use crate::native::{Runner, Trial};
#[cfg(not(target_os = "linux"))]
pub use crate::unsupported::{Runner, Trial};

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
    /// Where trials live: one directory per trial id.
    pub dir: PathBuf,
    pub backends: Backends,
    pub topology: Vec<CoreInfo>,
    /// The unprivileged account backends run as; without it every start is refused.
    pub identity: Option<BackendIdentity>,
    pub pm_table: Option<Arc<dyn PmTableReader + Send + Sync>>,
}
