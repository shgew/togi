//! Narrow machine-file publication operations used by fitting.

use std::{fs, path::Path};

use togi_sim::{MachineDefinition, file};

use crate::Error;

/// Output operations for fitted machines. `load` must decode the stored bytes: the ensemble checks
/// the encoded machine read back, not just its input definition.
pub trait MachineOutput: Send {
    fn prepare(&mut self, directory: &Path) -> Result<(), Error>;
    fn write(&mut self, path: &Path, content: &str, action: &'static str) -> Result<(), Error>;
    fn load(&self, path: &Path) -> Result<MachineDefinition, Error>;
}

pub(crate) struct DiskOutput;

impl MachineOutput for DiskOutput {
    fn prepare(&mut self, directory: &Path) -> Result<(), Error> {
        fs::create_dir_all(directory).map_err(|source| Error::Io {
            action: "create output directory",
            path: directory.to_owned(),
            source,
        })
    }

    fn write(&mut self, path: &Path, content: &str, action: &'static str) -> Result<(), Error> {
        fs::write(path, content).map_err(|source| Error::Io {
            action,
            path: path.to_owned(),
            source,
        })
    }

    fn load(&self, path: &Path) -> Result<MachineDefinition, Error> {
        file::load(path).map_err(|source| Error::Load {
            path: path.to_owned(),
            source,
        })
    }
}
