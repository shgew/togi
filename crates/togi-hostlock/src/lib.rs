#![cfg(unix)]

mod error;
mod policy;

use std::ffi::OsStr;
use std::fs::{self, File, TryLockError};
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{CWD, FileType, Mode, OFlags, fchmod, fstat, openat};
use rustix::io::Errno;
use rustix::process::geteuid;

pub use error::{DirectoryProblem, Error, LockProblem, Operation};
use policy::{Access, LockFacts, Mend, Place, directory_problem, lock_verdict};

#[cfg(target_os = "linux")]
pub const DEFAULT_PATH: &str = "/run/lock/togi.lock";
#[cfg(target_os = "macos")]
pub const DEFAULT_PATH: &str = "/tmp/togi.lock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Host,
    Private,
}

#[derive(Debug)]
pub struct HostLock {
    _file: File,
    path: PathBuf,
}

impl HostLock {
    pub fn acquire(path: &Path, policy: Policy) -> Result<Self, Error> {
        let (parent, name) = split(path)?;
        let io = |operation, source: io::Error| Error::Io {
            path: path.to_owned(),
            operation,
            source,
        };
        let resolved =
            fs::canonicalize(parent).map_err(|source| io(Operation::ResolveParent, source))?;
        let caller = geteuid().as_raw();
        let directory = open_containing_directory(&resolved, path, caller)?;
        let (file, access) = open_lock(&directory, name, path, policy)?;
        secure_lock(&file, path, policy, access)?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                _file: file,
                path: path.to_owned(),
            }),
            Err(TryLockError::WouldBlock) => Err(Error::Busy {
                path: path.to_owned(),
            }),
            Err(TryLockError::Error(source)) => Err(io(Operation::Lock, source)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn split(path: &Path) -> Result<(&Path, &OsStr), Error> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = path.file_name().ok_or_else(|| Error::Io {
        path: path.to_owned(),
        operation: Operation::ResolveParent,
        source: io::Error::new(io::ErrorKind::InvalidInput, "path names no file"),
    })?;
    Ok((parent, name))
}

fn io_error(path: &Path, operation: Operation, errno: Errno) -> Error {
    Error::Io {
        path: path.to_owned(),
        operation,
        source: io::Error::from(errno),
    }
}

fn open_containing_directory(resolved: &Path, lock: &Path, caller: u32) -> Result<OwnedFd, Error> {
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut at = PathBuf::from("/");
    let mut directory = openat(CWD, "/", directory_flags, Mode::empty())
        .map_err(|errno| io_error(lock, Operation::OpenDirectory, errno))?;
    let mut names = resolved
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .peekable();
    let mut descended = false;
    loop {
        let place = match (names.peek(), descended) {
            (None, _) => Place::Containing,
            (Some(_), false) => Place::NamespaceRoot,
            (Some(_), true) => Place::Ancestor,
        };
        let stat =
            fstat(&directory).map_err(|errno| io_error(lock, Operation::StatDirectory, errno))?;
        let mode = Mode::from_raw_mode(stat.st_mode);
        if let Some(problem) = directory_problem(mode, stat.st_uid, caller, place) {
            return Err(Error::UnsafeDirectory {
                path: lock.to_owned(),
                directory: at,
                uid: stat.st_uid,
                mode,
                problem,
            });
        }
        let Some(name) = names.next() else {
            return Ok(directory);
        };
        let flags = directory_flags | OFlags::NOFOLLOW;
        directory = openat(&directory, name, flags, Mode::empty())
            .map_err(|errno| io_error(lock, Operation::OpenDirectory, errno))?;
        at.push(name);
        descended = true;
    }
}

fn open_lock(
    directory: &OwnedFd,
    name: &OsStr,
    path: &Path,
    policy: Policy,
) -> Result<(File, Access), Error> {
    let base = OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let open = |flags: OFlags| openat(directory, name, base | flags, Mode::empty());
    let open_error = |errno| match errno {
        Errno::ISDIR | Errno::LOOP => Error::UnsafeLock {
            path: path.to_owned(),
            problem: LockProblem::NotRegularFile,
        },
        errno => io_error(path, Operation::Open, errno),
    };
    let read_write = match open(OFlags::RDWR) {
        Err(Errno::NOENT) => {
            if policy == Policy::Host && !geteuid().is_root() {
                return Err(Error::NotProvisioned {
                    path: path.to_owned(),
                });
            }
            let create = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL;
            match openat(directory, name, base | create, Mode::RUSR | Mode::WUSR) {
                Err(Errno::EXIST) => open(OFlags::RDWR),
                Err(errno) => return Err(io_error(path, Operation::Create, errno)),
                created => created,
            }
        }
        other => other,
    };
    match read_write {
        Ok(fd) => Ok((File::from(fd), Access::ReadWrite)),
        Err(Errno::ACCESS | Errno::PERM) => open(OFlags::RDONLY)
            .map(|fd| (File::from(fd), Access::ReadOnly))
            .map_err(open_error),
        Err(errno) => Err(open_error(errno)),
    }
}

fn secure_lock(file: &File, path: &Path, policy: Policy, access: Access) -> Result<(), Error> {
    let stat = fstat(file).map_err(|errno| io_error(path, Operation::Stat, errno))?;
    let facts = LockFacts {
        file_type: FileType::from_raw_mode(stat.st_mode),
        single_link: stat.st_nlink == 1,
        owner: stat.st_uid,
        mode: Mode::from_raw_mode(stat.st_mode),
    };
    match lock_verdict(facts, geteuid().as_raw(), policy, access) {
        Ok(Mend::Keep) => Ok(()),
        Ok(Mend::Restrict) => fchmod(file, Mode::RUSR | Mode::WUSR)
            .map_err(|errno| io_error(path, Operation::Chmod, errno)),
        Err(problem) => Err(Error::UnsafeLock {
            path: path.to_owned(),
            problem,
        }),
    }
}

#[cfg(test)]
mod tests;
