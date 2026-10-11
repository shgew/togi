use crate::Error;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

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
pub(crate) fn read_lossy(path: &Path) -> Result<String, Error> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(TEXT_LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|source| unreadable(path, source))?;
    if u64::try_from(bytes.len()).is_ok_and(|len| len > TEXT_LIMIT) {
        return Err(too_large(path, TEXT_LIMIT));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether `error` is a read failure because the file does not exist.
pub(crate) fn is_missing(error: &Error) -> bool {
    matches!(error, Error::ReadFile { source, .. } if source.kind() == io::ErrorKind::NotFound)
}

pub(crate) fn read_trimmed(path: &Path) -> Result<String, Error> {
    read_lossy(path).map(|text| text.trim().to_owned())
}

fn parse_int(path: &Path, text: &str) -> Result<i64, Error> {
    let text = text.trim();
    text.parse().map_err(|source| Error::ParseInt {
        path: path.to_owned(),
        text: text.to_owned(),
        source,
    })
}

pub(crate) fn read_int(path: &Path) -> Result<i64, Error> {
    parse_int(path, &read_lossy(path)?)
}

pub(crate) fn read_int_opt(path: &Path) -> Result<Option<i64>, Error> {
    match read_lossy(path) {
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

pub(crate) fn first_stanza(path: &Path) -> Result<Stanza, Error> {
    let read = || -> Result<Vec<u8>, Error> {
        let file = File::open(path).map_err(|source| unreadable(path, source))?;
        let mut reader = BufReader::new(file.take(STANZA_LIMIT + 1));
        let mut stanza = Vec::new();
        let mut line = Vec::new();
        let mut consumed = 0_u64;
        loop {
            line.clear();
            let count = reader
                .read_until(b'\n', &mut line)
                .map_err(|source| unreadable(path, source))?;
            consumed += u64::try_from(count).unwrap_or(u64::MAX);
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
