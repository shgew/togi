//! `togi-tool-facts`: extracts the privacy-safe trial facts of a copied state directory into a gzip extract,
//! the evidence the simulator replays.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use togi_journal::Recording;
use togi_tool_trialfacts::{
    ExtractError, Extracted, TrialFact, WriteError, encode, extract_dir, from_recordings, write,
};

#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error("read the state directory {}: {source}", path.display())]
    StateDir { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Write(#[from] WriteError),
}

/// What [`generate`] wrote and what reading the journals passed over.
#[derive(Debug)]
pub struct Generated {
    /// How many facts the extract holds.
    pub count: usize,
    /// One line per session whose journal held events of a kind this build does not know or unknown fields, saying
    /// which and where; empty when every journal was read whole.
    pub skipped: Vec<String>,
}

/// Extracts the facts of the state directory `state_dir` and writes them to `output`. The journals are read
/// leniently, so an event of an unknown kind is left out of the facts and named in [`Generated::skipped`].
/// `state_dir` must already be a directory: extraction takes its writer lock, which would otherwise create
/// the missing path. `output` is replaced only once the whole extract is written and synced: the extract
/// goes to a temporary file in the same directory first, which is removed on every failure. A failed run
/// leaves an existing `output` untouched.
pub fn generate(
    state_dir: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<Generated, GenerateError> {
    let state_dir = state_dir.as_ref();
    require_directory(state_dir).map_err(|source| GenerateError::StateDir {
        path: state_dir.to_path_buf(),
        source,
    })?;
    finish(extract_dir(state_dir), |facts| write(output, facts))
}

/// Extracts journals already held in memory and writes their gzip extract to `writer`.
/// Extraction completes before any bytes are written; the caller owns publication of the result.
pub fn generate_recordings(
    recordings: impl IntoIterator<Item = Recording>,
    writer: impl io::Write,
) -> Result<Generated, GenerateError> {
    finish(from_recordings(recordings), |facts| encode(writer, facts))
}

fn finish(
    extracted: Result<Extracted, ExtractError>,
    persist: impl FnOnce(&[TrialFact]) -> Result<(), WriteError>,
) -> Result<Generated, GenerateError> {
    let extracted = extracted?;
    persist(&extracted.facts)?;
    Ok(Generated {
        count: extracted.facts.len(),
        skipped: extracted
            .skipped
            .iter()
            .map(|(session, skipped)| format!("session {session}: {skipped}"))
            .collect(),
    })
}

fn require_directory(path: &Path) -> io::Result<()> {
    if fs::metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(io::ErrorKind::NotADirectory.into())
    }
}

#[cfg(test)]
#[path = "../tests/facts/fixture.rs"]
mod fixture;

#[cfg(test)]
#[path = "../tests/facts/generate.rs"]
mod tests;
