//! Preserves completed trials' samples and backend logs in gzip form and opens either stored form.

use std::ffi::OsString;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use rustix::fs::OFlags;
use rustix::io::Errno;

pub use goerr::{Cause, LinkError, PathError};

mod goerr;
mod goos;
mod gzip;
#[cfg(test)]
mod tests;

/// Names a trial's conditions samples.
pub const SAMPLES: &str = "samples.jsonl";

const INSTANCE_LOGS: [&str; 2] = ["stdout.log", "stderr.log"];

/// Go's `io.Copy` buffer size.
const COPY_BUFFER: usize = 32 * 1024;

/// One failed step of compressing a trial's files. The text is the one Go's wrapped error has, because it lands in the
/// journal verbatim: the operation, the file's base name and the `os` error with Go's errno text.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("list trial files: {0}")]
    List(#[source] PathError),
    #[error("open {name}: {source}")]
    Open { name: String, source: PathError },
    #[error("stat {name}: {source}")]
    Stat { name: String, source: PathError },
    #[error("remove stale {name}: {source}")]
    RemoveStale { name: String, source: PathError },
    #[error("create {name}: {source}")]
    Create { name: String, source: PathError },
    #[error("compress {name}: {source}")]
    Compress { name: String, source: Cause },
    #[error("sync compressed {name}: {source}")]
    SyncCompressed { name: String, source: Cause },
    #[error("remove plain {name}: {source}")]
    RemovePlain { name: String, source: PathError },
    #[error("sync removal of {name}: {source}")]
    SyncRemoval { name: String, source: Cause },
}

impl Error {
    /// The bytes of Go's `err.Error()`, for the journal. A path in the text keeps its raw bytes, so the text may not be
    /// UTF-8; the journal encoder must serialize it with Go's JSON rules: each invalid byte as the escape `\ufffd`, a
    /// real U+FFFD character as its UTF-8 bytes. `Display` shows each invalid byte as the character U+FFFD instead,
    /// which cannot tell the two apart.
    #[must_use]
    pub fn go_text(&self) -> Vec<u8> {
        let (prefix, cause) = match self {
            Error::List(source) => ("list trial files: ".to_owned(), source.go_text()),
            Error::Open { name, source } => (format!("open {name}: "), source.go_text()),
            Error::Stat { name, source } => (format!("stat {name}: "), source.go_text()),
            Error::RemoveStale { name, source } => {
                (format!("remove stale {name}: "), source.go_text())
            }
            Error::Create { name, source } => (format!("create {name}: "), source.go_text()),
            Error::Compress { name, source } => (format!("compress {name}: "), source.go_text()),
            Error::SyncCompressed { name, source } => {
                (format!("sync compressed {name}: "), source.go_text())
            }
            Error::RemovePlain { name, source } => {
                (format!("remove plain {name}: "), source.go_text())
            }
            Error::SyncRemoval { name, source } => {
                (format!("sync removal of {name}: "), source.go_text())
            }
        };
        let mut text = prefix.into_bytes();
        text.extend(cause);
        text
    }
}

/// Every error [`compress`] met, in the order it met them; the text joins them one per line.
#[derive(Debug)]
pub struct Errors(Vec<Error>);

impl Errors {
    /// The bytes of Go's `errors.Join(...).Error()`: each error's [`Error::go_text`] joined by `\n`.
    #[must_use]
    pub fn go_text(&self) -> Vec<u8> {
        self.0
            .iter()
            .map(Error::go_text)
            .collect::<Vec<_>>()
            .join(&b'\n')
    }

    #[must_use]
    pub fn errors(&self) -> &[Error] {
        &self.0
    }
}

impl fmt::Display for Errors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, error) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Errors {}

/// Finishes or retries compression after all writers in `dir` have stopped.
/// Missing files and directories are harmless: a trial may have stopped before creating them.
pub fn compress(dir: &Path) -> Result<(), Errors> {
    let entries = match goos::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.is_not_exist() => return Ok(()),
        Err(source) => return Err(Errors(vec![Error::List(source)])),
    };
    let mut errors = Vec::new();
    errors.extend(compress_file(&goerr::join(dir, SAMPLES.as_ref()), sync_dir).err());
    for (name, file_type) in entries {
        if !file_type.is_dir() {
            continue;
        }
        let instance = goerr::join(dir, &name);
        for log in INSTANCE_LOGS {
            errors.extend(compress_file(&goerr::join(&instance, log.as_ref()), sync_dir).err());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Errors(errors))
    }
}

/// Replaces the regular file at `path` with its gzip form at `path` + ".gz". The compressed file is written whole
/// under a temporary name, synced and renamed into place before the plain file is removed, so a reader finds one
/// complete form at every moment and a crash leaves either both (readers prefer the plain one) or the compressed one.
/// A missing path is skipped. So is anything that is not a regular file: a workload may replace its logs with a
/// symlink, which is never followed. `sync_dir` makes a change to path's directory durable. `path` is a Go-cleaned
/// path (see [`goerr::join`]), because error texts name paths the way Go does.
fn compress_file(
    path: &Path,
    mut sync_dir: impl FnMut(&Path) -> Result<(), Cause>,
) -> Result<(), Error> {
    let name = goerr::base(path);
    let mut src = match OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits().cast_signed())
        .open(path)
    {
        Ok(src) => src,
        Err(source)
            if source.kind() == io::ErrorKind::NotFound
                || source.raw_os_error() == Some(Errno::LOOP.raw_os_error()) =>
        {
            return Ok(());
        }
        Err(source) => {
            return Err(Error::Open {
                name,
                source: PathError::new("open", path, &source),
            });
        }
    };
    match src.metadata() {
        Ok(info) if info.is_file() => {}
        Ok(_) => return Ok(()),
        Err(source) => {
            return Err(Error::Stat {
                name,
                source: PathError::new("stat", path, &source),
            });
        }
    }
    let final_path = with_suffix(path, ".gz");
    let tmp = with_suffix(&final_path, ".tmp");
    if let Err(source) = goos::remove(&tmp)
        && !source.is_not_exist()
    {
        return Err(Error::RemoveStale {
            name: goerr::base(&tmp),
            source,
        });
    }
    let dst = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(OFlags::NOFOLLOW.bits().cast_signed())
        .open(&tmp)
        .map_err(|source| Error::Create {
            name: goerr::base(&tmp),
            source: PathError::new("open", &tmp, &source),
        })?;
    let published = write_gzip(&mut src, path, dst, &tmp)
        .and_then(|()| goos::rename_over(&tmp, &final_path).map_err(Cause::from));
    if let Err(source) = published {
        let _ = goos::remove(&tmp);
        return Err(Error::Compress { name, source });
    }
    let dir = goerr::dir(path);
    sync_dir(&dir).map_err(|source| Error::SyncCompressed {
        name: name.clone(),
        source,
    })?;
    goos::remove(path).map_err(|source| Error::RemovePlain {
        name: name.clone(),
        source,
    })?;
    sync_dir(&dir).map_err(|source| Error::SyncRemoval { name, source })
}

/// Copies `src` into the gzip file `dst`, syncs it and checks its close, in Go's order. Each failure names the file and
/// operation Go's `os.File` method would: `read` of the source, `write`, `sync` and `close` of the temporary.
fn write_gzip(src: &mut File, src_path: &Path, dst: File, dst_path: &Path) -> Result<(), Cause> {
    let write_error = |source: io::Error| PathError::new("write", dst_path, &source);
    let mut encoder = GzEncoder::new(dst, Compression::default());
    let mut buffer = vec![0; COPY_BUFFER];
    loop {
        match src.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => encoder.write_all(&buffer[..read]).map_err(write_error)?,
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) => return Err(PathError::new("read", src_path, &source).into()),
        }
    }
    let dst = encoder.finish().map_err(write_error)?;
    dst.sync_all()
        .map_err(|source| PathError::new("sync", dst_path, &source))?;
    Ok(goos::close(dst, dst_path)?)
}

/// Syncs the directory `path`; its close error counts, as in Go.
fn sync_dir(path: &Path) -> Result<(), Cause> {
    let dir = File::open(path).map_err(|source| PathError::new("open", path, &source))?;
    let synced = dir
        .sync_all()
        .map_err(|source| PathError::new("sync", path, &source));
    let closed = goos::close(dir, path);
    synced?;
    Ok(closed?)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path);
    name.push(suffix);
    PathBuf::from(name)
}

/// A trial file opened in whichever form it has; dropping it closes the file.
#[derive(Debug)]
pub struct TrialFile(Form);

#[derive(Debug)]
enum Form {
    Plain(File),
    Compressed(Box<gzip::Reader>),
}

impl Read for TrialFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.0 {
            Form::Plain(file) => file.read(buf),
            Form::Compressed(decoder) => decoder.read(buf),
        }
    }
}

/// Opens the trial file at `path`, or its compressed form when the plain one is gone. Compression removes the
/// plain file only after the compressed one is complete, so one of the two is always readable.
pub fn open(path: &Path) -> io::Result<TrialFile> {
    match File::open(path) {
        Ok(file) => return Ok(TrialFile(Form::Plain(file))),
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        Err(_) => {}
    }
    let file = File::open(with_suffix(path, ".gz"))?;
    Ok(TrialFile(Form::Compressed(Box::new(gzip::Reader::new(
        file,
    )?))))
}
