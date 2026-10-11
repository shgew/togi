//! Preserves completed trials' samples and backend logs in gzip form and opens either stored form.

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use rustix::fs::OFlags;
use rustix::io::Errno;

#[cfg(test)]
mod tests;

/// Names a trial's conditions samples.
pub const SAMPLES: &str = "samples.jsonl";

const INSTANCE_LOGS: [&str; 2] = ["stdout.log", "stderr.log"];

/// One failed step of compressing a trial's files, naming the file it concerns.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("list trial files in {}: {source}", dir.display())]
    List { dir: PathBuf, source: io::Error },
    #[error("open {}: {source}", path.display())]
    Open { path: PathBuf, source: io::Error },
    #[error("stat {}: {source}", path.display())]
    Stat { path: PathBuf, source: io::Error },
    #[error("remove stale {}: {source}", path.display())]
    RemoveStale { path: PathBuf, source: io::Error },
    #[error("create {}: {source}", path.display())]
    Create { path: PathBuf, source: io::Error },
    #[error("compress {}: {source}", path.display())]
    Compress { path: PathBuf, source: io::Error },
    #[error("sync compressed {}: {source}", path.display())]
    SyncCompressed { path: PathBuf, source: io::Error },
    #[error("remove plain {}: {source}", path.display())]
    RemovePlain { path: PathBuf, source: io::Error },
    #[error("sync removal of {}: {source}", path.display())]
    SyncRemoval { path: PathBuf, source: io::Error },
}

/// Every error [`compress`] met, in the order it met them; the text joins them one per line.
#[derive(Debug)]
pub struct Errors(Vec<Error>);

impl Errors {
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
    let instances = match instance_dirs(dir) {
        Ok(instances) => instances,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(Errors(vec![Error::List {
                dir: dir.to_owned(),
                source,
            }]));
        }
    };
    let mut errors = Vec::new();
    errors.extend(compress_file(&dir.join(SAMPLES), sync_dir).err());
    for instance in instances {
        for log in INSTANCE_LOGS {
            errors.extend(compress_file(&instance.join(log), sync_dir).err());
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Errors(errors))
    }
}

/// The real (not symlinked) subdirectories of `dir`, sorted by name. An entry that vanishes while listing is skipped.
fn instance_dirs(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut instances = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => instances.push(entry.path()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    instances.sort();
    Ok(instances)
}

/// Replaces the regular file at `path` with its gzip form at `path` + ".gz". The compressed file is written whole
/// under a temporary name, synced and renamed into place before the plain file is removed, so a reader finds one
/// complete form at every moment and a crash leaves either both (readers prefer the plain one) or the compressed one.
/// A missing path is skipped. So is anything that is not a regular file: a workload may replace its logs with a
/// symlink, which is never followed. `sync_dir` makes a change to path's directory durable.
fn compress_file(
    path: &Path,
    mut sync_dir: impl FnMut(&Path) -> io::Result<()>,
) -> Result<(), Error> {
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
                path: path.to_owned(),
                source,
            });
        }
    };
    match src.metadata() {
        Ok(info) if info.is_file() => {}
        Ok(_) => return Ok(()),
        Err(source) => {
            return Err(Error::Stat {
                path: path.to_owned(),
                source,
            });
        }
    }
    let final_path = with_suffix(path, ".gz");
    let tmp = with_suffix(&final_path, ".tmp");
    if let Err(source) = fs::remove_file(&tmp)
        && source.kind() != io::ErrorKind::NotFound
    {
        return Err(Error::RemoveStale { path: tmp, source });
    }
    let dst = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(OFlags::NOFOLLOW.bits().cast_signed())
        .open(&tmp)
        .map_err(|source| Error::Create {
            path: tmp.clone(),
            source,
        })?;
    let published = write_gzip(&mut src, dst).and_then(|()| fs::rename(&tmp, &final_path));
    if let Err(source) = published {
        let _ = fs::remove_file(&tmp);
        return Err(Error::Compress {
            path: path.to_owned(),
            source,
        });
    }
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    sync_dir(dir).map_err(|source| Error::SyncCompressed {
        path: path.to_owned(),
        source,
    })?;
    fs::remove_file(path).map_err(|source| Error::RemovePlain {
        path: path.to_owned(),
        source,
    })?;
    sync_dir(dir).map_err(|source| Error::SyncRemoval {
        path: path.to_owned(),
        source,
    })
}

fn write_gzip(src: &mut File, dst: File) -> io::Result<()> {
    let mut encoder = GzEncoder::new(dst, Compression::default());
    io::copy(src, &mut encoder)?;
    encoder.finish()?.sync_all()
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
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
    Compressed(Box<MultiGzDecoder<BufReader<File>>>),
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
    Ok(TrialFile(Form::Compressed(Box::new(MultiGzDecoder::new(
        BufReader::new(file),
    )))))
}
