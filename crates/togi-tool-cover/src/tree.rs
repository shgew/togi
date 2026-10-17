//! The file reads the report makes: changed sources and the coverage build's dep-info files.

use std::io;
use std::path::Path;

/// Files the report reads, on disk or in memory.
pub trait Tree {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// The names of the entries of the directory at `path`.
    fn list(&self, path: &Path) -> io::Result<Vec<String>>;
}

/// The file system.
#[derive(Clone, Copy, Debug)]
pub struct DiskTree;

impl Tree for DiskTree {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn list(&self, path: &Path) -> io::Result<Vec<String>> {
        std::fs::read_dir(path)?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect()
    }
}
