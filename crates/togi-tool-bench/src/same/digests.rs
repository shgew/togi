//! What `--same` remembers of a finished session: the digest of its journals, its exit status and its time, under
//! the digest of the simulator that ran it, so a tree whose simulator did not change is not simulated again.

use std::cmp::Reverse;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use togi_machine::object_only;

use crate::cache::{touch, write_atomic};
use crate::suite::{RunSpec, hex};

const FORMAT: &str = "togi-bench-same-rs-v1";
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

    #[derive(Debug, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct Entry {
        inputs: String,
        record: SideRecord,
    }
}

/// The key of the sessions a cache holds: the format, the boot cap and the simulator's bytes, which carry the
/// code that ran them. The rest of a session's inputs are checked per record ([`inputs`]).
pub(crate) fn key(simulator: &str, max_boots: u32) -> String {
    let mut hash = Sha256::new();
    hash.update(format!(
        "{FORMAT} boots={max_boots}\nsimulator {simulator}\n"
    ));
    hex(&hash.finalize())
}

/// The digest of what a session's simulator reads besides its own binary: the session's identity, whether it
/// replays facts, and every file of its machine by name relative to its suite, the facts extract included.
pub(crate) fn inputs(spec: &RunSpec) -> String {
    let mut hash = Sha256::new();
    hash.update(format!(
        "run {} replay={}\nmachine {}\n",
        spec.name(),
        spec.replay.is_some(),
        spec.machine.as_ref().map_or_else(
            || "default".to_owned(),
            |machine| format!("{} {}", machine.name, machine.digest()),
        ),
    ));
    hex(&hash.finalize())
}

/// One tree's finished sessions, each under the digest of its inputs.
#[derive(Debug)]
pub struct DigestCache {
    dir: PathBuf,
}

impl DigestCache {
    /// The cache for `key` under `root`: marked recently used, the least recently used others beyond
    /// [`KEPT_TREES`] removed.
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
        Ok(Self { dir })
    }

    fn path(&self, spec: &RunSpec) -> PathBuf {
        self.dir
            .join(&spec.scenario)
            .join(format!("{}-{}.json", spec.split, spec.seed))
    }

    /// The record kept for `spec` if it was made from the same `inputs`.
    pub fn get(&self, spec: &RunSpec, inputs: &str) -> Option<SideRecord> {
        let bytes = fs::read(self.path(spec)).ok()?;
        let entry: Entry = serde_json::from_slice(&bytes).ok()?;
        (entry.inputs == inputs).then_some(entry.record)
    }

    pub fn put(&self, spec: &RunSpec, inputs: &str, record: &SideRecord) -> io::Result<()> {
        let entry = Entry {
            inputs: inputs.to_owned(),
            record: record.clone(),
        };
        write_atomic(&self.path(spec), &serde_json::to_vec(&entry)?)
    }
}
