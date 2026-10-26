//! The backend contract: [`Backend`] checks an external stress program, builds a [`Plan`] for a
//! workload and classifies its output as [`Line`] evidence for the trial runner.

mod error;
mod file_name;

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub use error::{Error, InvalidAffinityCpu};
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
    /// The program could not pin itself; `None` means the message supplied no recognizable CPU.
    AffinityError(Option<CpuId>),
    InvalidAffinity(InvalidAffinityCpu),
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
    /// The effective configuration togi generates for `workload`, independent of the instance
    /// directory and of which CPUs it runs on: what [`Backend::plan`] generates for the workload's
    /// thread count on CPUs numbered from 0, with the directory left out, encoded by
    /// [`configuration_bytes`]. It checks nothing on disk.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for a workload the backend does not run.
    fn configuration(&self, workload: &Workload) -> Result<Vec<u8>, Error>;
    /// Classifies one line of output; the line carries no line terminator.
    fn classify(&self, line: &str) -> Line;
}

/// The canonical bytes of launch arguments and generated inputs: the argument count and every
/// argument, then the input count and every input's name and contents, each length-prefixed, so no
/// two different launches encode alike.
#[must_use]
pub fn configuration_bytes(args: &[OsString], inputs: &[Input]) -> Vec<u8> {
    fn field(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
        bytes.extend_from_slice(value);
    }
    let mut bytes = Vec::new();
    field(&mut bytes, &(args.len() as u64).to_le_bytes());
    for arg in args {
        field(&mut bytes, arg.as_bytes());
    }
    field(&mut bytes, &(inputs.len() as u64).to_le_bytes());
    for input in inputs {
        field(&mut bytes, input.name.as_str().as_bytes());
        field(&mut bytes, &input.contents);
    }
    bytes
}

/// The logical CPUs `0..threads`: the CPU list a configuration is generated for.
#[must_use]
pub fn numbered_cpus(threads: u8) -> Vec<CpuId> {
    (0..u16::from(threads)).map(CpuId::from).collect()
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
