use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use rustix::fs::Mode;

use crate::Policy;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("another togi process holds the host lock {}", path.display())]
    Busy { path: PathBuf },
    #[error("host lock {} does not exist and only root may create it", path.display())]
    NotProvisioned { path: PathBuf },
    #[error(
        "host lock {}: directory {} (uid {uid}, mode {mode:o}) {problem}",
        path.display(),
        directory.display()
    )]
    UnsafeDirectory {
        path: PathBuf,
        directory: PathBuf,
        uid: u32,
        mode: Mode,
        problem: DirectoryProblem,
    },
    #[error("host lock {} {problem}", path.display())]
    UnsafeLock { path: PathBuf, problem: LockProblem },
    #[error(transparent)]
    RefusedEntry(RefusedEntry),
    #[error("{operation} host lock {}", path.display())]
    Io {
        path: PathBuf,
        operation: Operation,
        source: io::Error,
    },
}

impl Error {
    pub(crate) fn unsafe_lock(path: &Path, policy: Policy, problem: LockProblem) -> Self {
        let path = path.to_owned();
        match (policy, problem) {
            (Policy::Host, LockProblem::ForeignOwner | LockProblem::NotRegularFile) => {
                Self::RefusedEntry(RefusedEntry { path, problem })
            }
            (
                Policy::Host,
                LockProblem::MultipleLinks
                | LockProblem::LegacyModeNotOwner
                | LockProblem::ReadOnly,
            )
            | (Policy::Private, _) => Self::UnsafeLock { path, problem },
        }
    }
}

/// The Linux host-lock policy refused its final entry as foreign-owned or nonregular.
/// It proves a refusal, not that another writer is absent.
#[derive(Debug, thiserror::Error)]
#[error("host lock {} {problem}", path.display())]
pub struct RefusedEntry {
    path: PathBuf,
    problem: LockProblem,
}

impl RefusedEntry {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn problem(&self) -> LockProblem {
        self.problem
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryProblem {
    #[error("is writable by group or others without the sticky bit")]
    WritableWithoutSticky,
    #[error("is owned by another user")]
    ForeignOwner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LockProblem {
    #[error("is not a regular file")]
    NotRegularFile,
    #[error("has more than one link")]
    MultipleLinks,
    #[error("is owned by another user")]
    ForeignOwner,
    #[error("has an unsupported mode and only its owner may restrict it")]
    LegacyModeNotOwner,
    #[error("is opened read-only; read-write access is required")]
    ReadOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    ResolveParent,
    OpenDirectory,
    StatDirectory,
    Open,
    Create,
    Stat,
    Chmod,
    Lock,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ResolveParent => "resolve the parent directory of",
            Self::OpenDirectory => "open a directory on the way to",
            Self::StatDirectory => "stat a directory on the way to",
            Self::Open => "open",
            Self::Create => "create",
            Self::Stat => "stat",
            Self::Chmod => "restrict permissions of",
            Self::Lock => "lock",
        })
    }
}
