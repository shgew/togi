//! The `os` package behaviors `trialfiles.Compress` relies on, with Go's error values: `Remove` (which also removes an
//! empty directory), `Rename`, `ReadDir` and a checked `File.Close`.

use std::ffi::OsString;
use std::fs::{self, File, FileType};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rustix::fs::{rename, rmdir, unlink};
use rustix::io::Errno;

use crate::goerr::{LinkError, PathError, errno_of};

/// Retries `call` while it is interrupted, like Go's `ignoringEINTR`.
fn uninterrupted(mut call: impl FnMut() -> Result<(), Errno>) -> Result<(), Errno> {
    loop {
        match call() {
            Err(Errno::INTR) => {}
            result => return result,
        }
    }
}

/// `os.Remove`: unlink, then rmdir. Symlinks are never followed; the error is unlink's unless rmdir failed for a reason
/// other than the path not being a directory.
pub(crate) fn remove(path: &Path) -> Result<(), PathError> {
    let Err(unlink_error) = uninterrupted(|| unlink(path)) else {
        return Ok(());
    };
    let Err(rmdir_error) = uninterrupted(|| rmdir(path)) else {
        return Ok(());
    };
    let errno = if rmdir_error == Errno::NOTDIR {
        unlink_error
    } else {
        rmdir_error
    };
    Err(PathError::from_errno("remove", path, errno.raw_os_error()))
}

/// `os.Rename`, including its refusal (EEXIST) to replace a directory.
pub(crate) fn rename_over(old: &Path, new: &Path) -> Result<(), LinkError> {
    if let Ok(new_info) = fs::symlink_metadata(new)
        && new_info.is_dir()
    {
        match fs::symlink_metadata(old) {
            Err(error) => return Err(LinkError::rename(old, new, errno_of(&error))),
            Ok(old_info) => {
                let same = old_info.dev() == new_info.dev() && old_info.ino() == new_info.ino();
                if old == new || !same {
                    return Err(LinkError::rename(old, new, Errno::EXIST.raw_os_error()));
                }
            }
        }
    }
    uninterrupted(|| rename(old, new))
        .map_err(|errno| LinkError::rename(old, new, errno.raw_os_error()))
}

/// `File.Close` with its error, which dropping the file would discard.
pub(crate) fn close(file: File, path: &Path) -> Result<(), PathError> {
    nix::unistd::close(file).map_err(|errno| PathError::from_errno("close", path, errno as i32))
}

/// `os.ReadDir` with the file type of each entry, sorted by name. An entry that vanished between listing and its
/// type lookup is skipped. Errors name `dir` as given.
pub(crate) fn read_dir(dir: &Path) -> Result<Vec<(OsString, FileType)>, PathError> {
    let listing = fs::read_dir(dir).map_err(|error| PathError::new("open", dir, &error))?;
    let mut entries = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|error| PathError::new("readdirent", dir, &error))?;
        match entry.file_type() {
            Ok(file_type) => entries.push((entry.file_name(), file_type)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                let mut path = dir.as_os_str().to_os_string();
                path.push("/");
                path.push(entry.file_name());
                return Err(PathError::new("fstatat", Path::new(&path), &error));
            }
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}
