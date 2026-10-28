//! Build-bound, integrity-checked caches of journal reducers. No snapshot authorizes a journal write.

use std::fmt::Write as _;
use std::io::{self, Read};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use togi_machine::object_only;

use crate::fs::Fs;
use crate::{Seq, StateError};

object_only! {
/// The exact complete journal prefix a snapshot covers, including line terminators.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prefix {
    pub last_seq: Option<Seq>,
    pub bytes: u64,
    pub sha256: String,
}
}

impl Prefix {
    /// Fingerprints exact journal bytes; loading also checks the complete-line and sequence boundary.
    pub fn of(bytes: &[u8], last_seq: Option<Seq>) -> Self {
        let mut hash = PrefixHash::default();
        hash.append(bytes);
        hash.prefix(last_seq)
    }
}

/// The incremental raw-byte fingerprint kept by the writer.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrefixHash {
    digest: Sha256,
    bytes: u64,
}

impl PrefixHash {
    pub(crate) fn append(&mut self, bytes: &[u8]) {
        self.digest.update(bytes);
        self.bytes += bytes.len() as u64;
    }

    pub(crate) fn prefix(&self, last_seq: Option<Seq>) -> Prefix {
        Prefix {
            last_seq,
            bytes: self.bytes,
            sha256: hex(&self.digest.clone().finalize()),
        }
    }
}

object_only! {
#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct Seal {
    build: String,
    prefix: Prefix,
    sha256: String,
}
}

/// Why a cache must be discarded. Readers fall back without writing; a run records the rebuild.
#[derive(Debug, thiserror::Error)]
pub enum Rejected {
    #[error("decode snapshot")]
    Decode(#[from] serde_json::Error),
    #[error("snapshot is not an object")]
    Shape,
    #[error("snapshot has no verification stamp")]
    Missing,
    #[error("snapshot belongs to another build")]
    Build,
    #[error("snapshot content checksum differs")]
    Content,
    #[error("snapshot journal prefix differs")]
    Prefix,
    #[error("snapshot reducer representation differs from this build's shape")]
    ReducerShape,
    #[error("read snapshot journal prefix")]
    Read(#[from] io::Error),
}

/// Adds a verification stamp while preserving the document's readable top-level fields.
///
/// # Errors
///
/// Returns an encoding error if the document is not representable as a JSON object.
pub fn seal(prefix: Prefix, document: &impl Serialize) -> Result<Value, StateError> {
    let mut value = serde_json::to_value(document).map_err(StateError::Encode)?;
    let bytes = serde_json::to_vec(&(env!("TOGI_SNAPSHOT_BUILD"), &prefix, &value))
        .map_err(StateError::Encode)?;
    let stamp = Seal {
        build: env!("TOGI_SNAPSHOT_BUILD").into(),
        prefix,
        sha256: hex(&Sha256::digest(bytes)),
    };
    let stamp = serde_json::to_value(stamp).map_err(StateError::Encode)?;
    let object = value.as_object_mut().ok_or_else(|| {
        StateError::Encode(<serde_json::Error as serde::ser::Error>::custom(
            "snapshot document is not an object",
        ))
    })?;
    object.insert("snapshot".into(), stamp);
    Ok(value)
}

/// Verifies the build and content before decoding any reducer state.
///
/// # Errors
///
/// Returns the reason the snapshot must not be used.
pub fn unseal<T: DeserializeOwned + Serialize>(value: Value) -> Result<(Prefix, T), Rejected> {
    let (prefix, value) = checked_document(value)?;
    Ok((prefix, decode_state(&value)?))
}

fn checked_document(mut value: Value) -> Result<(Prefix, Value), Rejected> {
    let object = value.as_object_mut().ok_or(Rejected::Shape)?;
    let seal: Seal = serde_json::from_value(object.remove("snapshot").ok_or(Rejected::Missing)?)?;
    if seal.build != env!("TOGI_SNAPSHOT_BUILD") {
        return Err(Rejected::Build);
    }
    let bytes = serde_json::to_vec(&(&seal.build, &seal.prefix, &value))?;
    if seal.sha256 != hex(&Sha256::digest(bytes)) {
        return Err(Rejected::Content);
    }
    Ok((seal.prefix, value))
}

fn decode_state<T: DeserializeOwned + Serialize>(value: &Value) -> Result<T, Rejected> {
    let state = T::deserialize(value)?;
    let encoded = serde_json::to_value(&state)?;
    let matches = match (value, &encoded) {
        (Value::Object(original), Value::Object(encoded)) => encoded
            .iter()
            .all(|(key, field)| original.get(key) == Some(field)),
        _ => *value == encoded,
    };
    if !matches {
        return Err(Rejected::ReducerShape);
    }
    Ok(state)
}

/// Checks the exact prefix without decoding or reducing its events. The caller may then fold only its tail.
///
/// # Errors
///
/// Returns an I/O failure or a mismatched prefix.
#[expect(
    clippy::naive_bytecount,
    reason = "counting prefix line boundaries needs no extra dependency"
)]
pub fn verify_prefix(fs: &dyn Fs, journal: &Path, prefix: &Prefix) -> Result<(), Rejected> {
    let mut file = fs.open_read(journal)?.take(prefix.bytes);
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    let mut lines = 0_u64;
    let mut last = None;
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        lines += buffer[..count]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count() as u64;
        last = buffer.get(count - 1).copied();
        digest.update(&buffer[..count]);
    }
    if bytes != prefix.bytes
        || lines != prefix.last_seq.map_or(0, Seq::get)
        || (bytes != 0 && last != Some(b'\n'))
        || hex(&digest.finalize()) != prefix.sha256
    {
        return Err(Rejected::Prefix);
    }
    Ok(())
}

/// Loads a cache without taking a lock or changing files. Rejection is never a journal failure.
///
/// # Errors
///
/// Returns the cache rejection; a missing cache is `Ok(None)`.
pub fn read_in<T: DeserializeOwned + Serialize>(
    fs: &dyn Fs,
    directory: &Path,
) -> Result<Option<(Prefix, T)>, Rejected> {
    let bytes = match fs.read(&directory.join("state.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    verified(fs, directory, serde_json::from_slice(&bytes)?).map(Some)
}

/// Checks a document read from `directory`'s `state.json`: build and content, then the journal prefix, then the
/// reducer shape.
fn verified<T: DeserializeOwned + Serialize>(
    fs: &dyn Fs,
    directory: &Path,
    document: Value,
) -> Result<(Prefix, T), Rejected> {
    let (prefix, state) = checked_document(document)?;
    verify_prefix(fs, &directory.join("events.jsonl"), &prefix)?;
    Ok((prefix, decode_state(&state)?))
}

/// The disk counterpart of [`read_in`], with the same read-only contract.
///
/// # Errors
///
/// Returns the cache rejection.
pub fn read<T: DeserializeOwned + Serialize>(
    directory: &Path,
) -> Result<Option<(Prefix, T)>, Rejected> {
    read_in(&crate::fs::DiskFs::default(), directory)
}

impl crate::StateDir {
    /// Reads a verified cache without opening its journal for writing.
    ///
    /// # Errors
    ///
    /// Returns the cache rejection.
    pub fn read_snapshot<T: DeserializeOwned + Serialize>(
        &self,
    ) -> Result<Option<(Prefix, T)>, Rejected> {
        read_in(self.fs(), self.path())
    }

    /// The checks of [`Self::read_snapshot`] for a `state.json` document the caller already read from this
    /// directory, so a reader that needs the raw document parses it once.
    ///
    /// # Errors
    ///
    /// Returns the cache rejection.
    pub fn verify_snapshot_document<T: DeserializeOwned + Serialize>(
        &self,
        document: Value,
    ) -> Result<(Prefix, T), Rejected> {
        verified(self.fs(), self.path(), document)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
