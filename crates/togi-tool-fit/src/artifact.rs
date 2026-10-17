//! The original research artifact of [the model program](../../../tools/fit/program.md#the-data): the first
//! seven real sessions, which model research develops and confirms on. Every run reads its extract through
//! [`load`], which keeps the sessions recorded after them out unless the run names the current extract.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use togi_journal::SessionId;
use togi_tool_trialfacts::{TrialFact, parse, read_rows};

use crate::error::Error;

/// The development sessions, oldest first.
pub const DEVELOPMENT: [&str; 6] = [
    "20260924T204352Z",
    "20260926T151414Z",
    "20260927T221954Z",
    "20260929T180308Z",
    "20261002T004254Z",
    "20261002T235548Z",
];

/// The artifact's newest session: a historical confirmation, scored before and not blind.
pub const CONFIRMATION: &str = "20261004T000701Z";

/// SHA-256 of the artifact's decompressed rows in extract order.
pub const SHA256: &str = "4b47590e12bd55bc73ee3ae468a782173acbafd4599892ef003e2371a8c066b7";

/// Which sessions a run may read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Sessions {
    /// None recorded after [`CONFIRMATION`], and an extract holding any artifact session must be the exact
    /// artifact. Research campaigns run only in this scope.
    Artifact,
    /// Every session of the extract: the owner-approved fit or check of the current evidence.
    Current,
}

/// An extract as read, with the identity of its rows.
#[derive(Debug)]
pub struct Extract {
    pub facts: Vec<TrialFact>,
    /// SHA-256 of the decompressed rows, in lowercase hex.
    pub sha256: String,
    pub rows: usize,
    pub sessions: usize,
}

impl Extract {
    pub fn is_artifact(&self) -> bool {
        self.sha256 == SHA256
    }
}

/// Reads the extract at `path` and refuses it when `sessions` does not allow what it holds.
pub fn load(path: &Path, sessions: Sessions) -> Result<Extract, Error> {
    admit(path, &read_rows(path)?, sessions)
}

/// Parses the decompressed `rows` of the extract at `path` and refuses them when `sessions` does not allow
/// what they hold.
pub(crate) fn admit(path: &Path, rows: &[u8], sessions: Sessions) -> Result<Extract, Error> {
    let facts = parse(rows)?;
    let names = session_names(&facts);
    let extract = Extract {
        sha256: sha256(rows),
        rows: facts.len(),
        sessions: names.len(),
        facts,
    };
    if sessions == Sessions::Artifact {
        if let Some(later) = names.values().find(|name| name.as_str() > CONFIRMATION) {
            return Err(Error::LaterSession {
                path: path.to_owned(),
                session: later.clone(),
            });
        }
        if !extract.is_artifact() && names.values().any(|name| is_artifact_session(name)) {
            return Err(Error::NotTheArtifact {
                path: path.to_owned(),
                sha256: extract.sha256,
            });
        }
    }
    Ok(extract)
}

/// Writes the artifact's rows from the extract at `source` to a new gzip file `out`, in the source's order,
/// after checking that they hash to [`SHA256`], and returns how many rows it wrote. An existing `out` is
/// never replaced; a partial file left by a failed write fails [`load`]'s hash check.
pub fn materialize(source: &Path, out: &Path) -> Result<usize, Error> {
    let (kept, count) = select(source, &read_rows(source)?)?;
    let io = |action: &'static str| {
        move |source| Error::Io {
            action,
            path: out.to_owned(),
            source,
        }
    };
    let file = File::create_new(out).map_err(io("create the artifact"))?;
    let mut gzip = GzEncoder::new(BufWriter::new(file), Compression::default());
    gzip.write_all(&kept).map_err(io("write the artifact"))?;
    gzip.finish()
        .and_then(|mut file| file.flush())
        .map_err(io("write the artifact"))?;
    Ok(count)
}

/// The artifact sessions' rows of the decompressed `rows` of the extract at `source`, in their order, and
/// how many there are; an error unless they hash to [`SHA256`].
pub(crate) fn select(source: &Path, rows: &[u8]) -> Result<(Vec<u8>, usize), Error> {
    let facts = parse(rows)?;
    let names = session_names(&facts);
    let mut kept = Vec::with_capacity(rows.len());
    let mut count = 0;
    for (line, fact) in rows.split_inclusive(|&byte| byte == b'\n').zip(&facts) {
        if names
            .get(&fact.session)
            .is_some_and(|name| is_artifact_session(name))
        {
            kept.extend_from_slice(line);
            if !line.ends_with(b"\n") {
                kept.push(b'\n');
            }
            count += 1;
        }
    }
    let hash = sha256(&kept);
    if hash != SHA256 {
        return Err(Error::ArtifactSource {
            path: source.to_owned(),
            sha256: hash,
        });
    }
    Ok((kept, count))
}

fn is_artifact_session(name: &str) -> bool {
    name == CONFIRMATION || DEVELOPMENT.contains(&name)
}

/// Each distinct session of `facts` with its name.
fn session_names(facts: &[TrialFact]) -> BTreeMap<SessionId, String> {
    let mut names = BTreeMap::new();
    for fact in facts {
        names
            .entry(fact.session)
            .or_insert_with(|| fact.session.to_string());
    }
    names
}

fn sha256(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}
