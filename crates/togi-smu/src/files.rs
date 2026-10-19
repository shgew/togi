use crate::Error;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;
use std::sync::Arc;

type ReadFile = dyn Fn(&Path) -> io::Result<Vec<u8>> + Send + Sync;
type WriteFile = dyn Fn(&Path, &[u8]) -> io::Result<usize> + Send + Sync;

pub(crate) struct Readers {
    pub(crate) cpu_names: Vec<String>,
    pub(crate) read: Arc<ReadFile>,
    pub(crate) write: Arc<WriteFile>,
}

#[derive(Clone)]
pub(crate) enum Files {
    System,
    Readers(Arc<Readers>),
}

impl Files {
    pub(crate) fn read(&self, path: &Path, limit: u64, capacity: usize) -> io::Result<Vec<u8>> {
        match self {
            Self::System => read_limited(File::open(path)?, limit, capacity),
            Self::Readers(readers) => {
                let mut bytes = (readers.read)(path)?;
                if let Ok(limit) = usize::try_from(limit) {
                    bytes.truncate(limit);
                }
                Ok(bytes)
            }
        }
    }
}

pub(crate) fn read_limited(input: impl Read, limit: u64, capacity: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(capacity);
    input.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

const TEXT_LIMIT: u64 = 64 * 1024;
const STANZA_LIMIT: u64 = 1024 * 1024;

fn unreadable(path: &Path, source: io::Error) -> Error {
    Error::ReadFile {
        path: path.to_owned(),
        source,
    }
}

fn too_large(path: &Path, limit: u64) -> Error {
    Error::TooLarge {
        path: path.to_owned(),
        limit,
    }
}

/// The file's text, lossy UTF-8; a file longer than the limit is refused, never truncated.
pub(crate) fn read_lossy(files: &Files, path: &Path) -> Result<String, Error> {
    let bytes = files
        .read(path, TEXT_LIMIT + 1, 0)
        .map_err(|source| unreadable(path, source))?;
    if bytes.len() as u64 > TEXT_LIMIT {
        return Err(too_large(path, TEXT_LIMIT));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether `error` is a read failure because the file does not exist.
pub(crate) fn is_missing(error: &Error) -> bool {
    matches!(error, Error::ReadFile { source, .. } if source.kind() == io::ErrorKind::NotFound)
}

pub(crate) fn read_trimmed(files: &Files, path: &Path) -> Result<String, Error> {
    read_lossy(files, path).map(|text| text.trim().to_owned())
}

fn parse_int(path: &Path, text: &str) -> Result<i64, Error> {
    let text = text.trim();
    text.parse().map_err(|source| Error::ParseInt {
        path: path.to_owned(),
        text: text.to_owned(),
        source,
    })
}

pub(crate) fn read_int(files: &Files, path: &Path) -> Result<i64, Error> {
    parse_int(path, &read_lossy(files, path)?)
}

pub(crate) fn read_int_opt(files: &Files, path: &Path) -> Result<Option<i64>, Error> {
    match read_lossy(files, path) {
        Ok(text) => parse_int(path, &text).map(Some),
        Err(error) if is_missing(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The `key : value` lines before the first blank line of a `/proc/cpuinfo`-style file.
#[derive(Debug, Default)]
pub(crate) struct Stanza {
    fields: Vec<(String, String)>,
}

impl Stanza {
    pub(crate) fn parse(text: &str) -> Self {
        let mut fields = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                fields.push((key.trim().to_owned(), value.trim().to_owned()));
            }
        }
        Self { fields }
    }

    pub(crate) fn get(&self, key: &str) -> &str {
        self.fields
            .iter()
            .find(|(name, _)| name == key)
            .map_or("", |(_, value)| value)
    }
}

pub(crate) fn first_stanza(files: &Files, path: &Path) -> Result<Stanza, Error> {
    match files {
        Files::System => {
            let file = File::open(path).map_err(|source| unreadable(path, source))?;
            stanza_from_reader(path, file)
        }
        Files::Readers(readers) => {
            let bytes = (readers.read)(path).map_err(|source| unreadable(path, source))?;
            stanza_from_reader(path, bytes.as_slice())
        }
    }
}

fn stanza_from_reader(path: &Path, input: impl Read) -> Result<Stanza, Error> {
    let read = || -> Result<Vec<u8>, Error> {
        let mut reader = BufReader::new(input.take(STANZA_LIMIT + 1));
        let mut stanza = Vec::new();
        let mut line = Vec::new();
        let mut consumed = 0_u64;
        loop {
            line.clear();
            let count = reader
                .read_until(b'\n', &mut line)
                .map_err(|source| unreadable(path, source))?;
            consumed += count as u64;
            if consumed > STANZA_LIMIT {
                return Err(too_large(path, STANZA_LIMIT));
            }
            if count == 0 || line.trim_ascii().is_empty() {
                return Ok(stanza);
            }
            stanza.extend_from_slice(&line);
        }
    };
    Ok(Stanza::parse(&String::from_utf8_lossy(&read()?)))
}
