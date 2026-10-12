use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::SystemTime;

use serde::Serialize;
use sha2::{Digest, Sha256};
use togi_machine::object_only;

use crate::record::Record;
use crate::suite::{RunSpec, hex};

const FORMAT: &str = "togi-bench-runs-v1";
const KEPT_TREES: usize = 8;
const KEPT_COST_SCOPES: usize = 8;
const TOUCH_ATTEMPTS: u32 = 3;

/// The key of the sessions a cache holds: the format, the boot cap, the simulator's bytes, which carry the code
/// that ran them, and the bench's own bytes, which carry the code that measured them. A change to either binary
/// or to a flag that shapes a session opens a new cache; the rest of a run's inputs are checked per record
/// ([`inputs`]).
pub fn tree_key(simulator: &str, bench: &str, max_boots: u32) -> String {
    let mut hash = Sha256::new();
    hash.update(format!(
        "{FORMAT} boots={max_boots}\nsimulator {simulator}\nbench {bench}\n"
    ));
    hex(&hash.finalize())
}

/// The digest of what one run reads besides the two binaries: the suite file, the run's identity and replay
/// setting, and every file of its machine, its facts extract included.
pub fn inputs(suite: &str, spec: &RunSpec) -> String {
    let mut hash = Sha256::new();
    hash.update(format!(
        "suite {suite}\nrun {} replay={}\nmachine {}\n",
        spec.name(),
        spec.replay.is_some(),
        spec.machine.as_ref().map_or_else(
            || "default".to_owned(),
            |machine| format!("{} {}", machine.name, machine.digest()),
        ),
    ));
    hex(&hash.finalize())
}

/// The SHA-256 of the running executable, in lower-case hex: the bench's own build.
pub fn current_exe_digest() -> io::Result<String> {
    let bytes = fs::read(std::env::current_exe()?)?;
    Ok(hex(&Sha256::digest(bytes)))
}

object_only! {
    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Entry {
        inputs: String,
        record: Record,
    }
}

/// One tree's records of finished sessions, each under the digest of its inputs.
#[derive(Debug)]
pub struct RunCache {
    dir: PathBuf,
}

impl RunCache {
    /// The cache for `key` under `root`: marked recently used, the least recently used others beyond
    /// [`KEPT_TREES`] removed.
    pub fn open(root: &Path, key: &str) -> io::Result<Self> {
        let parent = root.join("runs");
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
    pub fn get(&self, spec: &RunSpec, inputs: &str) -> Option<Record> {
        let bytes = fs::read(self.path(spec)).ok()?;
        let entry: Entry = serde_json::from_slice(&bytes).ok()?;
        let same_run = entry.record.scenario == spec.scenario
            && entry.record.seed == spec.seed
            && entry.record.split == spec.split;
        (entry.inputs == inputs && same_run).then_some(entry.record)
    }

    pub fn put(&self, spec: &RunSpec, inputs: &str, record: &Record) -> io::Result<()> {
        let entry = Entry {
            inputs: inputs.to_owned(),
            record: record.clone(),
        };
        write_atomic(&self.path(spec), &serde_json::to_vec(&entry)?)
    }
}

/// Creates `dir` and marks it used now. Another bench pruning its least recently used caches may remove `dir`
/// between the two, so a directory found missing is created again, a few times at most.
pub(crate) fn touch(dir: &Path) -> io::Result<()> {
    touch_with(dir, |dir| File::open(dir)?.set_modified(SystemTime::now()))
}

fn touch_with(dir: &Path, mut mark: impl FnMut(&Path) -> io::Result<()>) -> io::Result<()> {
    for attempt in 1.. {
        fs::create_dir_all(dir)?;
        match mark(dir) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() != ErrorKind::NotFound || attempt == TOUCH_ATTEMPTS => {
                return Err(error);
            }
            Err(_) => {}
        }
    }
    unreachable!("the loop returns on the last attempt")
}

pub(crate) fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(content)?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// How long each session took, to start the longest first. Costs are kept per scope, the simulator that ran
/// them, since a session takes very different times on different simulators; a session never run by a scope's
/// simulator takes the cost of its latest run by any.
#[derive(Debug)]
pub struct Costs {
    path: PathBuf,
    data: Mutex<CostFile>,
}

object_only! {
    #[derive(Debug, Default, Serialize)]
    #[serde(deny_unknown_fields)]
    struct CostFile {
        latest: BTreeMap<String, f64>,
        scopes: Vec<CostScope>,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct CostScope {
        key: String,
        wall: BTreeMap<String, f64>,
    }
}

impl Costs {
    /// The costs saved at `path`; none when the file is absent or unreadable.
    pub fn load(path: PathBuf) -> Self {
        let data = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            data: Mutex::new(data),
        }
    }

    fn data(&self) -> MutexGuard<'_, CostFile> {
        self.data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn set(&self, scope: &str, session: &str, wall: f64) {
        if wall <= 0.0 || !wall.is_finite() {
            return;
        }
        let mut data = self.data();
        data.latest.insert(session.to_owned(), wall);
        if !data.scopes.iter().any(|s| s.key == scope) {
            data.scopes.insert(
                0,
                CostScope {
                    key: scope.to_owned(),
                    wall: BTreeMap::new(),
                },
            );
            data.scopes.truncate(KEPT_COST_SCOPES);
        }
        if let Some(entry) = data.scopes.iter_mut().find(|s| s.key == scope) {
            entry.wall.insert(session.to_owned(), wall);
        }
    }

    pub fn get(&self, scope: &str, session: &str) -> Option<f64> {
        let data = self.data();
        data.scopes
            .iter()
            .find(|s| s.key == scope)
            .and_then(|s| s.wall.get(session))
            .or_else(|| data.latest.get(session))
            .copied()
    }

    pub fn save(&self) -> io::Result<()> {
        let bytes = serde_json::to_vec(&*self.data())?;
        write_atomic(&self.path, &bytes)
    }
}

/// The indexes `0..n` by decreasing cost. Those without a known cost come first, in their given order, as they
/// may be the longest; equal costs keep their order.
pub fn longest_first(n: usize, cost: impl Fn(usize) -> Option<f64>) -> Vec<usize> {
    let costs: Vec<f64> = (0..n).map(|i| cost(i).unwrap_or(f64::INFINITY)).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| costs[b].total_cmp(&costs[a]));
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_removing(dir: &Path, removals: u32) -> (io::Result<()>, u32) {
        let mut calls = 0;
        let result = touch_with(dir, |dir| {
            calls += 1;
            if calls <= removals {
                fs::remove_dir_all(dir).unwrap();
            }
            File::open(dir)?.set_modified(SystemTime::now())
        });
        (result, calls)
    }

    #[test]
    fn a_directory_pruned_before_it_is_marked_is_created_again() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("runs/key");
        let (result, calls) = touch_removing(&dir, 1);
        result.unwrap();
        assert_eq!(calls, 2);
        assert!(dir.is_dir());
    }

    #[test]
    fn a_directory_pruned_on_every_attempt_fails_after_the_last() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("runs/key");
        let (result, calls) = touch_removing(&dir, u32::MAX);
        assert_eq!(result.unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(calls, TOUCH_ATTEMPTS);
    }
}
