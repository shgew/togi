//! `togi-tool-facts`: extracts the privacy-safe trial facts of a copied state directory into a gzip extract,
//! the evidence the simulator replays.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use togi_tool_trialfacts::{ExtractError, WriteError, extract_dir, write};

#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error("read the state directory {}: {source}", path.display())]
    StateDir { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Write(#[from] WriteError),
}

/// Extracts the facts of the state directory `state_dir` and writes them to `output`, returning how many.
/// `state_dir` must already be a directory: extraction takes its writer lock, which would otherwise create
/// the missing path. `output` is replaced only once the whole extract is written and synced: the extract
/// goes to a temporary file in the same directory first, which is removed on every failure. A failed run
/// leaves an existing `output` untouched.
pub fn generate(
    state_dir: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<usize, GenerateError> {
    let state_dir = state_dir.as_ref();
    require_directory(state_dir).map_err(|source| GenerateError::StateDir {
        path: state_dir.to_path_buf(),
        source,
    })?;
    let facts = extract_dir(state_dir)?;
    write(output, &facts)?;
    Ok(facts.len())
}

fn require_directory(path: &Path) -> io::Result<()> {
    if fs::metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(io::ErrorKind::NotADirectory.into())
    }
}
