mod error;
mod file_name;

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub use error::Error;
pub use file_name::{FileName, InvalidFileName};
use togi_machine::{CpuId, Workload};

/// One line of a backend's output, as the trial runner acts on it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Line {
    Other,
    /// What passed, as the trial reports it.
    Progress(String),
    ComputationError,
    SetupError,
    /// The program could not pin itself to a CPU; the CPU is unknown when its number does not parse.
    AffinityError(Option<CpuId>),
}

/// What a passing `Backend::check` shows in preflight.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ready {
    pub detail: String,
}

/// A generated input file; the trial creates it in the instance directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Input {
    pub name: FileName,
    pub contents: Vec<u8>,
}

/// How to launch one backend instance. Adapters write nothing: the trial materializes the
/// inputs and launches `program` with `args` in the instance directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub inputs: Vec<Input>,
    /// Files in the instance directory the runner tails for errors.
    pub watched: Vec<FileName>,
}

pub trait Backend: Send + Sync {
    fn kind(&self) -> togi_machine::Backend;
    fn check(&self) -> Result<Ready, Error>;
    /// `dir` is where the instance runs; `cpus` are the logical CPUs it may use, in order.
    fn plan(&self, workload: &Workload, dir: &Path, cpus: &[CpuId]) -> Result<Plan, Error>;
    /// Classifies one line of output; the line carries no line terminator.
    fn classify(&self, line: &str) -> Line;
}

/// Checks that the named backend's binary at `path` is a regular file with an execute bit.
///
/// Follows symlinks. A binary that does not exist is `Error::Missing`.
pub fn check_executable(name: &'static str, path: &Path) -> Result<(), Error> {
    let metadata = std::fs::metadata(path).map_err(|source| {
        let path = path.to_path_buf();
        if source.kind() == io::ErrorKind::NotFound {
            Error::Missing { name, path, source }
        } else {
            Error::Stat { name, path, source }
        }
    })?;
    if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
        Ok(())
    } else {
        Err(Error::NotExecutable {
            name,
            path: path.to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests;
