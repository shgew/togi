//! Copies of a state directory for forecast, facts and replay: the files a resumed session reads and nothing else,
//! the live journal, `state.json` when present and every archive entry that belongs to a session (archived
//! journals and the carry, compatibility and `reset --all` markers). Trial directories are left out.

use std::io;
use std::path::{Path, PathBuf};

use togi_journal::DirError;
use togi_journal::fs::{FileKind, Fs};

const LIVE: &str = "events.jsonl";
const STATE: &str = "state.json";
const ARCHIVE: &str = "archive";

#[derive(Debug, thiserror::Error)]
pub enum CopyError {
    #[error("list the archive of {}", dir.display())]
    List {
        dir: PathBuf,
        #[source]
        source: DirError,
    },
    #[error("{what} {}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Copies the state directory `src` into `dst` on disk, creating `dst` and its missing ancestors ([`copy_in`]).
/// The copy is a scratch input, so nothing in it is synced.
pub fn copy(src: &Path, dst: &Path) -> Result<(), CopyError> {
    #[expect(
        clippy::disallowed_methods,
        reason = "a state copy is a scratch input for forecast, facts and replay; nothing depends on it surviving a crash"
    )]
    let fs = togi_journal::fs::unsynced_disk_fs();
    copy_in(fs.as_ref(), src, dst)
}

/// [`copy`] over `fs`. `src` is only read, without taking its lock; every link in it is refused, never followed.
/// An archive entry named after a session that is not a regular file fails the copy, naming it, before anything is
/// written. Files already in `dst` with the same names are replaced.
pub fn copy_in(fs: &dyn Fs, src: &Path, dst: &Path) -> Result<(), CopyError> {
    let archived = togi_journal::session_entries_in(fs, src).map_err(|source| CopyError::List {
        dir: src.to_path_buf(),
        source,
    })?;
    let mut files = vec![PathBuf::from(LIVE)];
    let state = src.join(STATE);
    if fs.kind(&state).map_err(io_at("read", &state))?.is_some() {
        files.push(PathBuf::from(STATE));
    }
    files.extend(archived.iter().map(|name| Path::new(ARCHIVE).join(name)));
    create_dirs(fs, dst)?;
    if !archived.is_empty() {
        create_dirs(fs, &dst.join(ARCHIVE))?;
    }
    for name in files {
        let (from, to) = (src.join(&name), dst.join(&name));
        let bytes = fs.read(&from).map_err(io_at("read", &from))?;
        fs.create_truncate(&to)
            .and_then(|mut file| file.write_all(&bytes))
            .map_err(io_at("write", &to))?;
    }
    Ok(())
}

fn create_dirs(fs: &dyn Fs, dir: &Path) -> Result<(), CopyError> {
    if fs.kind(dir).map_err(io_at("read", dir))? == Some(FileKind::Dir) {
        return Ok(());
    }
    if let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        create_dirs(fs, parent)?;
    }
    match fs.create_dir(dir) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
            Err(io_at("create directory", dir)(error))
        }
        _ => Ok(()),
    }
}

fn io_at(what: &'static str, path: &Path) -> impl FnOnce(io::Error) -> CopyError {
    let path = path.to_path_buf();
    move |source| CopyError::Io { what, path, source }
}

#[cfg(test)]
mod tests;
