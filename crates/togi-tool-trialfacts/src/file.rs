use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;

use crate::record::{InvalidFact, TrialFact};

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("open extract {}: {source}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read extract: {0}")]
    Io(#[from] io::Error),
    #[error("extract line {line}: {source}")]
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
    #[error("fact {index} contradicts itself: {source}")]
    Invalid { index: usize, source: InvalidFact },
    #[error("write extract: {0}")]
    Io(#[from] io::Error),
    #[error("replace {}: {source}", path.display())]
    Replace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Reads a gzip extract of one JSON record per line. Any failure is an error and no records.
pub fn read(path: impl AsRef<Path>) -> Result<Vec<TrialFact>, ReadError> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|source| ReadError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    decode(file)
}

pub fn decode(reader: impl Read) -> Result<Vec<TrialFact>, ReadError> {
    let mut lines = BufReader::new(MultiGzDecoder::new(reader));
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

/// Writes `facts` as a gzip extract, replacing `path` only once every byte is written: the extract goes to
/// a temporary file beside it first. Without facts, nothing is created or changed.
pub fn write(path: impl AsRef<Path>, facts: &[TrialFact]) -> Result<(), WriteError> {
    let path = path.as_ref();
    validate(facts)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    encode(BufWriter::new(temporary.as_file_mut()), facts)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| WriteError::Replace {
            path: path.to_path_buf(),
            source: error.error,
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
