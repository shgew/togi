use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::cfg::HostCfg;
use crate::tree::Tree;

/// Files in memory by absolute path; a directory is any prefix of a file's path.
#[derive(Debug, Default)]
pub(crate) struct MemTree(BTreeMap<PathBuf, Vec<u8>>);

impl MemTree {
    /// Files named relative to `root`.
    pub(crate) fn new(root: &str, files: &[(&str, &str)]) -> Self {
        Self(
            files
                .iter()
                .map(|(name, content)| (Path::new(root).join(name), content.as_bytes().to_vec()))
                .collect(),
        )
    }
}

impl Tree for MemTree {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.0
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn list(&self, path: &Path) -> io::Result<Vec<String>> {
        let names: BTreeSet<String> = self
            .0
            .keys()
            .filter_map(|file| file.strip_prefix(path).ok()?.components().next())
            .map(|name| name.as_os_str().to_string_lossy().into_owned())
            .collect();
        if names.is_empty() {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        Ok(names.into_iter().collect())
    }
}

/// The cfg of a Linux host: what `rustc --print cfg` prints there, without the cargo llvm-cov option.
pub(crate) fn linux_host() -> HostCfg {
    HostCfg::new([
        "unix",
        r#"target_os="linux""#,
        r#"target_family="unix""#,
        r#"target_arch="x86_64""#,
        "debug_assertions",
    ])
}
