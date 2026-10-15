use std::{io, path::Path};

use togi_journal::fs::{FileKind, Fs, MemFs};
use togi_sim::{MachineDefinition, file};

use crate::{Error, MachineOutput};

#[derive(Clone, Debug, Default)]
pub(crate) struct MemoryOutput(pub MemFs);

impl MemoryOutput {
    pub(super) fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        if self.0.kind(path)? == Some(FileKind::Dir) {
            return Ok(());
        }
        if let Some(parent) = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty() && *p != path)
        {
            self.create_dir_all(parent)?;
        }
        self.0.create_dir(path)
    }

    pub(super) fn write(&self, path: &Path, content: &str) -> io::Result<()> {
        self.0.create_truncate(path)?.write_all(content.as_bytes())
    }

    pub(super) fn read_to_string(&self, path: &Path) -> io::Result<String> {
        String::from_utf8(self.0.read(path)?)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    pub(super) fn exists(&self, path: &Path) -> bool {
        self.0.kind(path).unwrap().is_some()
    }

    pub(super) fn is_dir(&self, path: &Path) -> bool {
        self.0.kind(path).unwrap() == Some(FileKind::Dir)
    }
}

impl MachineOutput for MemoryOutput {
    fn prepare(&mut self, path: &Path) -> Result<(), Error> {
        self.create_dir_all(path).map_err(|source| Error::Io {
            action: "create output directory",
            path: path.to_owned(),
            source,
        })
    }

    fn write(&mut self, path: &Path, content: &str, action: &'static str) -> Result<(), Error> {
        MemoryOutput::write(self, path, content).map_err(|source| Error::Io {
            action,
            path: path.to_owned(),
            source,
        })
    }

    fn load(&self, path: &Path) -> Result<MachineDefinition, Error> {
        let content = self.read_to_string(path).map_err(|source| Error::Io {
            action: "read fitted machine",
            path: path.to_owned(),
            source,
        })?;
        file::decode(&content).map_err(|source| Error::Load {
            path: path.to_owned(),
            source: file::LoadError::Parse {
                path: path.to_owned(),
                source,
            },
        })
    }
}
