//! A session's journals as `--same` compares them: the live journal and the archived ones, line by line, after
//! the build stamp is taken out of the two events that carry it.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::suite::hex;

const LIVE: &str = "events.jsonl";
const ARCHIVE: &str = "archive";
const SUFFIX: &str = ".jsonl";
const STAMPED: [&str; 2] = ["session.start", "config.loaded"];
const READ_BUFFER: usize = 1 << 20;

/// Why a session's journals could not be digested or compared.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("list journals in {}: {source}", path.display())]
    List {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read journal {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("journal {}: line {line}: decode event: {source}", path.display())]
    Decode {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("missing journals")]
    Missing,
    #[error("journals contain no events")]
    Empty,
    #[error("missing journals: base has {base}, head has {head}")]
    MissingOnASide { base: usize, head: usize },
    #[error("{side} journals contain no events")]
    EmptyOnASide { side: &'static str },
}

/// The first place two sessions' journals differ.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Difference {
    /// The journal, relative to the session's directory, or a name in angle brackets for a difference that is
    /// not in a journal.
    pub file: String,
    /// The one-based line of the journal; none for a difference that is not in a line.
    pub line: Option<usize>,
    pub base: String,
    pub head: String,
}

/// The live journal and the archived journals of a session's directory by path relative to it, oldest first by
/// name. Other files, such as the trials' samples, are not journals.
pub fn files(dir: &Path) -> Result<BTreeMap<String, PathBuf>, JournalError> {
    let mut files = BTreeMap::new();
    let live = dir.join(LIVE);
    match fs::metadata(&live) {
        Ok(meta) if meta.is_file() => {
            files.insert(LIVE.to_owned(), live);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(JournalError::List { path: live, source }),
    }
    let archive = dir.join(ARCHIVE);
    let entries = match fs::read_dir(&archive) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(files),
        Err(source) => {
            return Err(JournalError::List {
                path: archive,
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| JournalError::List {
            path: archive.clone(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_file = entry.metadata().is_ok_and(|meta| meta.is_file());
        if is_file && name.ends_with(SUFFIX) {
            files.insert(format!("{ARCHIVE}/{name}"), entry.path());
        }
    }
    Ok(files)
}

/// `line` without the build stamp: the line itself unless it is a `session.start` or `config.loaded` event, in
/// which case the build's `version` and `rev` are removed and the build's description is taken out of the
/// message that names it. The stamped event is compared as the JSON value it holds, so its key order and
/// spacing are not compared; every other line is compared byte for byte.
pub fn normalize(line: &[u8]) -> Result<Cow<'_, [u8]>, serde_json::Error> {
    if !STAMPED.iter().any(|kind| contains(line, kind.as_bytes())) {
        return Ok(Cow::Borrowed(line));
    }
    let mut event: Value = serde_json::from_slice(line)?;
    let Some(kind) = event
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| STAMPED.contains(kind))
        .map(str::to_owned)
    else {
        return Ok(Cow::Borrowed(line));
    };
    let Some(build) = event
        .pointer_mut("/data/build")
        .and_then(Value::as_object_mut)
    else {
        return Ok(Cow::Borrowed(line));
    };
    let version = text(build.remove("version").as_ref());
    let rev = text(build.remove("rev").as_ref());
    let description = describe(&version, &rev);
    let data = event.get("data");
    let session = text(data.and_then(|data| data.get("session")));
    let path = text(data.and_then(|data| data.get("path")));
    let file = data.and_then(|data| data.get("file")) == Some(&Value::Bool(true));
    if let Some(message) = event.get_mut("msg") {
        let prefix = match (kind.as_str(), file) {
            ("session.start", _) => format!("session {session} started by "),
            (_, true) => format!("config loaded from {path} by "),
            (_, false) => format!("no config at {path}; using defaults with "),
        };
        if let Some(rest) = message
            .as_str()
            .and_then(|message| message.strip_prefix(&format!("{prefix}{description}")))
            .filter(|rest| rest.starts_with(" (schema "))
        {
            *message = Value::String(format!("{prefix}{rest}"));
        }
    }
    let mut normalized = serde_json::to_vec(&event)?;
    if line.ends_with(b"\n") {
        normalized.push(b'\n');
    }
    Ok(Cow::Owned(normalized))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// How a build names itself in a message: `togi VERSION+REV`.
fn describe(version: &str, rev: &str) -> String {
    let mut description = "togi".to_owned();
    if !version.is_empty() {
        description.push(' ');
        description.push_str(version);
    }
    if !rev.is_empty() {
        description.push('+');
        description.push_str(rev);
    }
    description
}

/// The SHA-256, in lower-case hex, of a session directory's journals after build normalization, each with its
/// name and normalized size, so a session that kept another number of archives or other contents differs.
pub fn digest(dir: &Path) -> Result<String, JournalError> {
    let files = files(dir)?;
    if files.is_empty() {
        return Err(JournalError::Missing);
    }
    let mut total = Sha256::new();
    let mut nonempty = false;
    for (name, path) in &files {
        let (sum, size) = file_digest(path)?;
        nonempty |= size > 0;
        total.update(format!("{name} {size} {}\n", hex(&sum)));
    }
    if !nonempty {
        return Err(JournalError::Empty);
    }
    Ok(hex(&total.finalize()))
}

fn file_digest(path: &Path) -> Result<([u8; 32], u64), JournalError> {
    let read = |source| JournalError::Read {
        path: path.to_path_buf(),
        source,
    };
    let mut reader = BufReader::with_capacity(READ_BUFFER, File::open(path).map_err(read)?);
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut line = Vec::new();
    for number in 1.. {
        line.clear();
        if reader.read_until(b'\n', &mut line).map_err(read)? == 0 {
            break;
        }
        let normalized = normalize(&line).map_err(|source| JournalError::Decode {
            path: path.to_path_buf(),
            line: number,
            source,
        })?;
        hash.update(&normalized);
        size += normalized.len() as u64;
    }
    Ok((hash.finalize().into(), size))
}

/// The first line at which two journals differ after build normalization, with each side's text; a side that
/// ended first is `<missing event>`. The journals' lines are read one at a time.
pub fn first_difference(
    base: impl BufRead,
    head: impl BufRead,
) -> Result<Option<(usize, String, String)>, JournalError> {
    let mut readers: [Box<dyn BufRead>; 2] = [Box::new(base), Box::new(head)];
    let mut lines: [Vec<u8>; 2] = [Vec::new(), Vec::new()];
    for number in 1.. {
        for (reader, line) in readers.iter_mut().zip(&mut lines) {
            let mut raw = Vec::new();
            reader
                .read_until(b'\n', &mut raw)
                .map_err(|source| JournalError::Read {
                    path: PathBuf::new(),
                    source,
                })?;
            *line = if raw.is_empty() {
                raw
            } else {
                normalize(&raw)
                    .map_err(|source| JournalError::Decode {
                        path: PathBuf::new(),
                        line: number,
                        source,
                    })?
                    .into_owned()
            };
        }
        if lines[0] == lines[1] {
            if lines[0].is_empty() {
                return Ok(None);
            }
            continue;
        }
        let show = |line: &[u8]| {
            if line.is_empty() {
                "<missing event>".to_owned()
            } else {
                String::from_utf8_lossy(line)
                    .trim_end_matches('\n')
                    .to_owned()
            }
        };
        return Ok(Some((number, show(&lines[0]), show(&lines[1]))));
    }
    Ok(None)
}

/// The first difference between two session directories' journals: in the first journal, by name, that differs.
/// A journal one side lacks differs at its first line. It is an error for a side to have no journal or only
/// empty ones, so two failed sessions never compare equal.
pub fn compare_dirs(base: &Path, head: &Path) -> Result<Option<Difference>, JournalError> {
    let sides = [files(base)?, files(head)?];
    if sides[0].is_empty() || sides[1].is_empty() {
        return Err(JournalError::MissingOnASide {
            base: sides[0].len(),
            head: sides[1].len(),
        });
    }
    for (files, side) in sides.iter().zip(["base", "head"]) {
        let mut nonempty = false;
        for path in files.values() {
            let meta = fs::metadata(path).map_err(|source| JournalError::Read {
                path: path.clone(),
                source,
            })?;
            nonempty |= meta.len() > 0;
        }
        if !nonempty {
            return Err(JournalError::EmptyOnASide { side });
        }
    }
    let names: BTreeSet<&String> = sides.iter().flat_map(BTreeMap::keys).collect();
    for name in names {
        let paths = [sides[0].get(name), sides[1].get(name)];
        if let Some(difference) = compare_file(name, paths)? {
            return Ok(Some(difference));
        }
    }
    Ok(None)
}

fn compare_file(
    name: &str,
    paths: [Option<&PathBuf>; 2],
) -> Result<Option<Difference>, JournalError> {
    let open = |path: Option<&PathBuf>| -> Result<Box<dyn BufRead>, JournalError> {
        match path {
            None => Ok(Box::new(io::empty())),
            Some(path) => File::open(path)
                .map(|file| Box::new(BufReader::new(file)) as Box<dyn BufRead>)
                .map_err(|source| JournalError::Read {
                    path: path.clone(),
                    source,
                }),
        }
    };
    let found =
        first_difference(open(paths[0])?, open(paths[1])?).map_err(|error| match error {
            JournalError::Read { source, .. } => JournalError::Read {
                path: PathBuf::from(name),
                source,
            },
            JournalError::Decode { line, source, .. } => JournalError::Decode {
                path: PathBuf::from(name),
                line,
                source,
            },
            other => other,
        })?;
    let missing = [paths[0].is_none(), paths[1].is_none()];
    let (line, mut base, mut head) = match found {
        Some(found) => found,
        None if missing[0] || missing[1] => {
            (1, "<empty file>".to_owned(), "<empty file>".to_owned())
        }
        None => return Ok(None),
    };
    if missing[0] {
        "<missing file>".clone_into(&mut base);
    }
    if missing[1] {
        "<missing file>".clone_into(&mut head);
    }
    Ok(Some(Difference {
        file: name.to_owned(),
        line: Some(line),
        base,
        head,
    }))
}
