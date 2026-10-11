use crate::Error;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

const TEXT_LIMIT: u64 = 64 * 1024;
const STANZA_LIMIT: u64 = 1024 * 1024;

pub(crate) fn read_lossy(path: &Path) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(TEXT_LIMIT).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn unreadable(path: &Path, source: io::Error) -> Error {
    Error::ReadFile {
        path: path.to_owned(),
        source,
    }
}

pub(crate) fn read_trimmed(path: &Path) -> Result<String, Error> {
    read_lossy(path)
        .map(|text| text.trim().to_owned())
        .map_err(|source| unreadable(path, source))
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
    let text = read_lossy(path).map_err(|source| unreadable(path, source))?;
    parse_int(path, &text)
}

pub(crate) fn read_int_opt(path: &Path) -> Result<Option<i64>, Error> {
    match read_lossy(path) {
        Ok(text) => parse_int(path, &text).map(Some),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(unreadable(path, source)),
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
    let read = || -> io::Result<Vec<u8>> {
        let mut reader = BufReader::new(File::open(path)?.take(STANZA_LIMIT));
        let mut stanza = Vec::new();
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 || line.trim_ascii().is_empty() {
                return Ok(stanza);
            }
            stanza.extend_from_slice(&line);
        }
    };
    let bytes = read().map_err(|source| unreadable(path, source))?;
    Ok(Stanza::parse(&String::from_utf8_lossy(&bytes)))
}
