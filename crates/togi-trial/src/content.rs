//! SHA-256 digests of what a workload runs: the executable's bytes and the generated configuration.

use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest as _, Sha256};
use togi_journal::fs::Fs;
use togi_machine::Digest;

/// The digest of `bytes`.
pub(crate) fn bytes_digest(bytes: &[u8]) -> Digest {
    Digest::from_bytes(Sha256::digest(bytes).into())
}

/// The digest of the file at `path`, read in chunks so an executable of any size is never held in
/// memory. It reads through `fs` when the runner has one, and from the disk otherwise.
pub(crate) fn file_digest(fs: Option<&dyn Fs>, path: &Path) -> io::Result<Digest> {
    let mut source: Box<dyn Read + Send> = match fs {
        Some(fs) => fs.open_read(path)?,
        None => Box::new(std::fs::File::open(path)?),
    };
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 64 << 10];
    loop {
        match source.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => hasher.update(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Digest::from_bytes(hasher.finalize().into()))
}
