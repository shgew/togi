use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;

use crate::record::{InvalidFact, TrialFact};

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("open extract {}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read extract")]
    Io(#[from] io::Error),
    #[error("extract line {line}")]
    Line {
        line: u64,
        #[source]
        source: serde_json::Error,
    },
    #[error("the extract holds no records")]
    Empty,
}

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("no facts to write")]
    Empty,
    #[error("fact {index} records a voltage request that is not finite")]
    NotFinite { index: usize },
    #[error("fact {index} contradicts itself")]
    Invalid { index: usize, source: InvalidFact },
    #[error("write extract")]
    Io(#[from] io::Error),
    #[error("replace {}", path.display())]
    Replace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Reads a gzip extract of one JSON record per line. Any failure is an error and no records.
pub fn read(path: impl AsRef<Path>) -> Result<Vec<TrialFact>, ReadError> {
    parse(&read_rows(path)?)
}

/// The decompressed bytes of a gzip extract: its rows, which pin the extract by their hash.
pub fn read_rows(path: impl AsRef<Path>) -> Result<Vec<u8>, ReadError> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|source| ReadError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let mut rows = Vec::new();
    MultiGzDecoder::new(file).read_to_end(&mut rows)?;
    Ok(rows)
}

/// Parses an extract's decompressed rows, one JSON record per line.
pub fn parse(rows: &[u8]) -> Result<Vec<TrialFact>, ReadError> {
    records(rows)
}

pub fn decode(reader: impl Read) -> Result<Vec<TrialFact>, ReadError> {
    records(BufReader::new(MultiGzDecoder::new(reader)))
}

fn records(mut lines: impl BufRead) -> Result<Vec<TrialFact>, ReadError> {
    let mut facts = Vec::new();
    let mut line = Vec::new();
    for number in 1_u64.. {
        line.clear();
        if lines.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        facts.push(
            serde_json::from_slice(&line).map_err(|source| ReadError::Line {
                line: number,
                source,
            })?,
        );
    }
    if facts.is_empty() {
        return Err(ReadError::Empty);
    }
    Ok(facts)
}

/// Writes `facts` as a gzip extract, syncing every byte before atomically replacing `path`: the extract
/// goes to a temporary file beside it first. Without valid facts, nothing is created or changed.
pub fn write(path: impl AsRef<Path>, facts: &[TrialFact]) -> Result<(), WriteError> {
    write_file(path.as_ref(), facts, sync_file)
}

/// Like [`write()`], including atomic replacement, but without syncing the temporary file.
///
/// Use for disposable extracts whose survival across a machine crash is not required.
pub fn write_unsynced(path: impl AsRef<Path>, facts: &[TrialFact]) -> Result<(), WriteError> {
    write_file(path.as_ref(), facts, |_| Ok(()))
}

#[expect(
    clippy::disallowed_methods,
    reason = "production extracts must be synced before replacing the previous complete extract"
)]
fn sync_file(file: &File) -> io::Result<()> {
    file.sync_all()
}

fn write_file(
    path: &Path,
    facts: &[TrialFact],
    sync: impl FnOnce(&File) -> io::Result<()>,
) -> Result<(), WriteError> {
    write_with(
        path,
        facts,
        |parent| tempfile::NamedTempFile::new_in(parent),
        |temporary| sync(temporary.as_file()),
        |temporary, path| {
            temporary
                .persist(path)
                .map(|_| ())
                .map_err(|error| error.error)
        },
    )
}

fn write_with<T: Write>(
    path: &Path,
    facts: &[TrialFact],
    create: impl FnOnce(&Path) -> io::Result<T>,
    sync: impl FnOnce(&T) -> io::Result<()>,
    replace: impl FnOnce(T, &Path) -> io::Result<()>,
) -> Result<(), WriteError> {
    validate(facts)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = create(parent)?;
    encode(BufWriter::new(&mut temporary), facts)?;
    sync(&temporary)?;
    replace(temporary, path).map_err(|source| WriteError::Replace {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// The gzip bytes of `facts`, one JSON record per line, identical for identical facts.
pub fn encode(writer: impl Write, facts: &[TrialFact]) -> Result<(), WriteError> {
    validate(facts)?;
    let mut gzip = GzEncoder::new(writer, Compression::default());
    for fact in facts {
        serde_json::to_writer(&mut gzip, fact).map_err(io::Error::from)?;
        gzip.write_all(b"\n")?;
    }
    gzip.finish()?.flush()?;
    Ok(())
}

fn validate(facts: &[TrialFact]) -> Result<(), WriteError> {
    if facts.is_empty() {
        return Err(WriteError::Empty);
    }
    for (index, fact) in facts.iter().enumerate() {
        fact.check()
            .map_err(|source| WriteError::Invalid { index, source })?;
        if fact
            .voltage_requests_v
            .iter()
            .any(|(_, volts)| !volts.is_finite())
        {
            return Err(WriteError::NotFinite { index });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
