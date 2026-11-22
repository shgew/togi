use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use flate2::read::MultiGzDecoder;

use crate::record::TrialFact;

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

/// Reads a gzip extract of one JSON record per line. Any failure is an error and no records.
pub(super) fn read(path: &Path) -> Result<Vec<TrialFact>, ReadError> {
    let file = File::open(path).map_err(|source| ReadError::Open {
        path: path.to_path_buf(),
        source,
    })?;
    let mut rows = Vec::new();
    MultiGzDecoder::new(file).read_to_end(&mut rows)?;
    records(rows.as_slice())
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
