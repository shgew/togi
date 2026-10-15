//! Private host adapters for the bench orchestration; content tests replace these, not the orchestration.
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    Costs, Error, Fleet, Options, Record, Root, RunCache, RunError, RunSpec, Session, Simulator,
};
use crate::SuiteSource;
use crate::suite::DiskSource;

pub(super) trait RunRoot {
    fn path(&self) -> &Path;
    fn kept(&self) -> Option<PathBuf>;
    fn discard(&self);
}

pub(super) trait SessionRunner: Sync {
    fn digest(&self) -> &str;
    fn run(
        &self,
        fleet: &Fleet,
        root: &Path,
        spec: &RunSpec,
        boots: NonZeroU32,
        timeout: Duration,
    ) -> Result<Session, RunError>;
    fn remove(&self, path: &Path);
}

pub(super) trait ExecutionIo: SuiteSource + Sync {
    type Simulator: SessionRunner;
    type Root: RunRoot;
    fn simulator(&self, options: &Options) -> Result<Self::Simulator, Error>;
    fn cache(
        &self,
        options: &Options,
        simulator: &Self::Simulator,
    ) -> Result<(Option<RunCache>, Option<Costs>), Error>;
    fn root(&self, keep: Option<&Path>) -> Result<Self::Root, Error>;
    fn write_records(&self, path: &Path, records: &[Record], baseline: bool) -> Result<(), Error>;
    fn read_records(&self, path: &Path, what: &'static str) -> Result<Vec<Record>, Error> {
        let bytes = self.read(path).map_err(|source| Error::ReadFile {
            what,
            path: path.to_path_buf(),
            source,
        })?;
        crate::record::read_from(bytes.as_slice(), path, what)
    }
}

impl ExecutionIo for DiskSource {
    type Simulator = Simulator;
    type Root = Root;
    fn simulator(&self, options: &Options) -> Result<Simulator, Error> {
        Simulator::locate(options.simulator.as_deref())
    }
    fn cache(
        &self,
        options: &Options,
        simulator: &Simulator,
    ) -> Result<(Option<RunCache>, Option<Costs>), Error> {
        super::open_cache(options, simulator)
    }
    fn root(&self, keep: Option<&Path>) -> Result<Root, Error> {
        super::run_root(keep)
    }
    fn write_records(&self, path: &Path, records: &[Record], baseline: bool) -> Result<(), Error> {
        if baseline {
            crate::record::write_baseline(path, records)
        } else {
            crate::record::write(path, records)
        }
    }
    fn read_records(&self, path: &Path, what: &'static str) -> Result<Vec<Record>, Error> {
        crate::record::read(path, what)
    }
}

impl SessionRunner for Simulator {
    fn digest(&self) -> &str {
        self.digest()
    }
    fn run(
        &self,
        fleet: &Fleet,
        root: &Path,
        spec: &RunSpec,
        boots: NonZeroU32,
        timeout: Duration,
    ) -> Result<Session, RunError> {
        self.run(fleet, root, spec, boots, timeout)
    }
    fn remove(&self, path: &Path) {
        let _ = std::fs::remove_dir_all(path);
    }
}

impl RunRoot for Root {
    fn path(&self) -> &Path {
        self.path()
    }
    fn kept(&self) -> Option<PathBuf> {
        self.kept()
    }
    fn discard(&self) {
        self.discard();
    }
}
