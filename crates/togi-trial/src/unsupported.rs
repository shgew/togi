use std::convert::Infallible;

use togi_machine::{
    CancellationToken, ContentId, Partial, Reporter, Running, Started, Stopped, Swept,
    TrialConditions, TrialResult, TrialSpec, Trials, WorkloadId,
};

use crate::error::Error;
use crate::runner::Options;

fn unsupported() -> Error {
    Error::Unsupported {
        operation: "running trials",
    }
}

/// Trials run on Linux only; elsewhere a runner cannot be built, so no method below can run.
pub struct Runner(Infallible);

impl Runner {
    pub fn new(_options: Options) -> Result<Self, Error> {
        Err(unsupported())
    }

    pub fn new_unsynced(_options: Options) -> Result<Self, Error> {
        Err(unsupported())
    }

    pub fn probe_scope(&self) -> Result<String, Error> {
        match self.0 {}
    }
}

impl Trials for Runner {
    fn sweep(&mut self, _: &CancellationToken) -> Result<Swept, togi_machine::Error> {
        match self.0 {}
    }

    fn start(
        &mut self,
        _: &CancellationToken,
        _: TrialSpec,
        _: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        match self.0 {}
    }

    fn ended(&mut self, _: &str) -> Result<(), togi_machine::Error> {
        match self.0 {}
    }

    fn content(&mut self, _: WorkloadId) -> Result<ContentId, togi_machine::Error> {
        match self.0 {}
    }

    fn samples<'a>(
        &'a self,
        _: &'a str,
    ) -> Box<dyn Iterator<Item = Result<TrialConditions, togi_machine::SampleError>> + 'a> {
        match self.0 {}
    }

    fn controller_pid(&self) -> u32 {
        match self.0 {}
    }
}

/// A started trial; none exists where trials cannot run.
pub struct Trial(Infallible);

impl Running for Trial {
    fn started(&self) -> Started {
        match self.0 {}
    }

    fn wait(
        &mut self,
        _: &CancellationToken,
        _: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        match self.0 {}
    }

    fn stop(self: Box<Self>) -> Result<Stopped, togi_machine::Error> {
        match self.0 {}
    }
}
