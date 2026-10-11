use std::fmt;
use std::io;
use std::path::PathBuf;

use rustix::fs::Mode;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("another togi process holds the host lock {}", path.display())]
    Busy { path: PathBuf },
    #[error(
        "host lock {} does not exist and only root may create it: root must provision it",
        path.display()
    )]
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
    #[error("{operation} host lock {}: {source}", path.display())]
    Io {
        path: PathBuf,
        operation: Operation,
        source: io::Error,
    },
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
    #[error("has permissions other than 0600 or 0660 and only its owner may restrict them")]
    LegacyModeNotOwner,
    #[error("is opened read-only, which is not delegated access")]
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
