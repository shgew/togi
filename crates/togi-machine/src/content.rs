use serde::{Deserialize, Serialize};

use crate::Backend;

/// A SHA-256 digest as 64 lowercase hexadecimal digits.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{0:?} is not 64 lowercase hexadecimal digits")]
pub struct InvalidDigest(pub String);

impl Digest {
    pub fn new(hex: String) -> Result<Self, InvalidDigest> {
        if hex.len() == 64
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            Ok(Self(hex))
        } else {
            Err(InvalidDigest(hex))
        }
    }

    /// The digest of 32 raw bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        Self(
            bytes
                .iter()
                .flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0xf)]])
                .map(char::from)
                .collect(),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Digest {
    type Error = InvalidDigest;
    fn try_from(hex: String) -> Result<Self, InvalidDigest> {
        Self::new(hex)
    }
}

impl From<Digest> for String {
    fn from(digest: Digest) -> Self {
        digest.0
    }
}

/// What a workload actually runs: its backend, the executable's bytes and the configuration togi
/// generates for it, independent of instance directory and CPUs (`docs/spec/workloads.md`,
/// Evidence compatibility). A package store path is provenance, never part of this identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContentId {
    pub backend: Backend,
    pub executable: Digest,
    pub configuration: Digest,
}
