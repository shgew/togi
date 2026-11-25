//! What `--same` remembers of a finished session: the digest of its journals, its exit status and its time, under
//! the digest of the simulator that ran it, so a tree whose simulator did not change is not simulated again.

use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use togi_journal::fs::Fs;

use sha2::{Digest, Sha256};
use togi_machine::object_only;
use togi_tool_sim::scripted::ScriptedCase;

use crate::cache::{touch, write_atomic, write_in};
use crate::suite::hex;

const FORMAT: &str = "togi-bench-scripted-v1";
const KEPT_TREES: usize = 8;

object_only! {
    /// A finished session as `--same` compares it: the digest of its journals, the exit status of its simulator,
    /// and how long it ran.
    #[derive(Clone, Debug, PartialEq, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SideRecord {
        pub digest: String,
        pub exit: i32,
        pub wall_s: f64,
    }
}

/// The namespace of cached cases: the format, scripted input contract and simulator bytes.
pub(crate) fn key(simulator: &str, contract: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(format!(
        "{FORMAT} contract={contract}\nsimulator {simulator}\n"
    ));
    hex(&hash.finalize())
}

/// One tree's finished sessions, indexed by scripted case.
#[derive(Debug)]
pub struct DigestCache {
    dir: PathBuf,
    fs: Option<Arc<dyn Fs>>,
}

impl DigestCache {
    /// The cache for `key` under `root`: marked recently used, the least recently used others beyond
    /// `KEPT_TREES` removed.
    pub fn open(root: &Path, key: &str) -> io::Result<Self> {
        let parent = root.join("same");
        let dir = parent.join(key);
        touch(&dir)?;
        let mut others = Vec::new();
        for entry in fs::read_dir(&parent)? {
            let entry = entry?;
            if entry.file_name() != key
                && let Ok(modified) = entry.metadata().and_then(|meta| meta.modified())
                && entry.file_type()?.is_dir()
            {
                others.push((modified, entry.file_name()));
            }
        }
        others.sort_by_key(|entry| Reverse(entry.clone()));
        for (_, name) in others.into_iter().skip(KEPT_TREES - 1) {
            let _ = fs::remove_dir_all(parent.join(name));
        }
        Ok(Self { dir, fs: None })
    }

    /// Opens an already selected namespace through `fs`, without disk LRU bookkeeping.
    pub fn with_fs(root: &Path, key: &str, fs: Arc<dyn Fs>) -> Self {
        Self {
            dir: root.join("same").join(key),
            fs: Some(fs),
        }
    }

    fn path(&self, case: ScriptedCase) -> PathBuf {
        self.dir.join(format!("{}.json", case.name()))
    }

    /// The record kept for this case in this input namespace.
    pub fn get(&self, case: ScriptedCase) -> Option<SideRecord> {
        let path = self.path(case);
        let bytes = match &self.fs {
            Some(fs) => fs.read(&path),
            None => fs::read(path),
        }
        .ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn put(&self, case: ScriptedCase, record: &SideRecord) -> io::Result<()> {
        let path = self.path(case);
        let bytes = serde_json::to_vec(record)?;
        match &self.fs {
            None => write_atomic(&path, &bytes),
            Some(fs) => write_in(fs.as_ref(), &path, &bytes),
        }
    }
}
