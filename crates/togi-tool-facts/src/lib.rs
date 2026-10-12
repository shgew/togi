//! `togi-tool-facts`: extracts the privacy-safe trial facts of a copied state directory into a gzip extract,
//! the evidence the simulator replays.

use std::path::Path;

use togi_tool_trialfacts::{ExtractError, WriteError, extract_dir, write};

#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Write(#[from] WriteError),
}

/// Extracts the facts of the state directory `state_dir` and writes them to `output`, returning how many.
/// `output` is replaced only once the whole extract is written and synced: the extract goes to a temporary
/// file in the same directory first, which is removed on every failure. A failed run leaves an existing
/// `output` untouched.
pub fn generate(
    state_dir: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<usize, GenerateError> {
    let facts = extract_dir(state_dir)?;
    write(output, &facts)?;
    Ok(facts.len())
}
