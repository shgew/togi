//! File reads and writes whose failures name the operation and the path.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use crate::Error;

pub fn read(path: &str) -> Result<Vec<u8>, Error> {
    let mut file = File::open(path).map_err(|error| Error::path("open", Path::new(path), error))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| Error::path("read", Path::new(path), error))?;
    Ok(bytes)
}

/// Writes `bytes` to `path`, creating it readable by all.
pub fn write(path: &str, bytes: &[u8]) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(path)
        .map_err(|error| Error::path("open", Path::new(path), error))?;
    file.write_all(bytes)
        .map_err(|error| Error::path("write", Path::new(path), error))
}

pub fn create_dir_all(path: &str) -> Result<(), Error> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(path)
        .map_err(|error| Error::path("mkdir", Path::new(path), error))
}

/// Removes `path` and everything under it; a missing path is already removed.
pub fn remove_all(path: &str) -> Result<(), Error> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == ErrorKind::NotADirectory => {
            fs::remove_file(path).map_err(|error| Error::path("unlinkat", Path::new(path), error))
        }
        Err(error) if error.kind() != ErrorKind::NotFound => {
            Err(Error::path("unlinkat", Path::new(path), error))
        }
        _ => Ok(()),
    }
}
