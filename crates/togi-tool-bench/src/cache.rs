use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use serde::Serialize;
use togi_journal::fs::Fs;
use togi_machine::object_only;

const KEPT_COST_SCOPES: usize = 8;
const TOUCH_ATTEMPTS: u32 = 3;

pub(crate) fn write_in(fs: &dyn Fs, path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        create_dirs(fs, parent)?;
    }
    let temporary = path.with_extension("tmp");
    fs.create_truncate(&temporary)?.write_all(bytes)?;
    fs.rename(&temporary, path)
}

fn create_dirs(fs: &dyn Fs, path: &Path) -> io::Result<()> {
    use togi_journal::fs::FileKind;
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    match fs.kind(path)? {
        Some(FileKind::Dir) => return Ok(()),
        Some(_) => return Err(ErrorKind::NotADirectory.into()),
        None => {}
    }
    if let Some(parent) = path.parent() {
        create_dirs(fs, parent)?;
    }
    match fs.create_dir(path) {
        Err(error)
            if error.kind() == ErrorKind::AlreadyExists
                && fs.kind(path)? == Some(FileKind::Dir) =>
        {
            Ok(())
        }
        result => result,
    }
}

/// Creates `dir` and marks it used now. Another comparison may prune it between the two operations,
/// so a directory found missing is created again, a few times at most.
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
    fs: Option<Arc<dyn Fs>>,
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
            fs: None,
            data: Mutex::new(data),
        }
    }

    /// Loads the same cost format from an injected filesystem.
    pub fn with_fs(path: PathBuf, fs: Arc<dyn Fs>) -> Self {
        let data = fs
            .read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            fs: Some(fs),
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
        match &self.fs {
            Some(fs) => write_in(fs.as_ref(), &self.path, &bytes),
            None => write_atomic(&self.path, &bytes),
        }
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
    fn real_fs_a_directory_pruned_before_it_is_marked_is_created_again() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("runs/key");
        let (result, calls) = touch_removing(&dir, 1);
        result.unwrap();
        assert_eq!(calls, 2);
        assert!(dir.is_dir());
    }

    #[test]
    fn real_fs_a_directory_pruned_on_every_attempt_fails_after_the_last() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("runs/key");
        let (result, calls) = touch_removing(&dir, u32::MAX);
        assert_eq!(result.unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(calls, TOUCH_ATTEMPTS);
    }
}
