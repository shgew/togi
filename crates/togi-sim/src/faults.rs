//! Armed faults and the SMU interception hook.

use std::collections::{BTreeMap, BTreeSet};
use togi_machine::{Backend, CoreId, CoreSet, Error};

/// The kind of SMU access a hook sees.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmuOp {
    Read,
    Set,
    SetAll,
}

/// Whether a hook runs before an SMU access takes effect or after it has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookPhase {
    Before,
    After,
}

/// One access the hook is called for. `offset` is the register value read, or the value written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SmuOperation {
    pub op: SmuOp,
    /// `None` for an access to every core.
    pub core: Option<CoreId>,
    pub offset: i64,
    pub phase: HookPhase,
}

/// Called before and after every actual SMU access. An error before the access stops it; an error after
/// it is returned to the caller, but the access has happened and is not rolled back.
pub trait SmuHook {
    fn intercept(&mut self, operation: &SmuOperation) -> Result<(), Error>;
}

impl<F: FnMut(&SmuOperation) -> Result<(), Error>> SmuHook for F {
    fn intercept(&mut self, operation: &SmuOperation) -> Result<(), Error> {
        self(operation)
    }
}

#[derive(Default)]
pub(crate) struct Faults {
    /// Accepted writes left until the one that fails; zero is unarmed.
    pub fail_write_in: u32,
    /// Accepted writes left until the one that crashes the machine; zero is unarmed.
    pub crash_write_in: u32,
    /// First writes of the next boots that crash the machine.
    pub crash_before_apply: u32,
    pub wrote_this_boot: bool,
    pub corrupt_armed: CoreSet,
    pub corrupt_pending: CoreSet,
    pub fail_setup: u32,
    pub escape: bool,
    pub missing_backends: BTreeSet<Backend>,
    pub failed_checks: BTreeMap<String, String>,
    pub hook: Option<Box<dyn SmuHook>>,
}

impl Faults {
    pub(crate) fn start_boot(&mut self) {
        self.wrote_this_boot = false;
        self.corrupt_pending = CoreSet::default();
    }
}
