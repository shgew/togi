use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use serde::Serialize;
use togi_machine::object_only;
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::error::Error;
use crate::outcome::Outcome;
use crate::session::Anchor;
use crate::summary::{Summary, summarize};

object_only! {
    /// A machine file or facts extract the ensemble read, with its content hash.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct File {
        /// Relative to the suite.
        pub path: String,
        /// SHA-256 in lower-case hex.
        pub sha256: String,
    }
}

object_only! {
    /// One simulated run of the ensemble.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Run {
        pub machine: String,
        pub seed: u64,
        pub split: String,
        pub outcome: Outcome,
    }
}

object_only! {
    /// A committed forecast: the anchor it starts after, the build it was made by, the ensemble and its summary.
    /// It holds the anchor's identity and simulated outcomes only, no journal content, and is the same for the same
    /// inputs: no wall times and no local paths.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Record {
        pub anchor: Anchor,
        /// The short revision of the tree the forecast was made from.
        pub commit: String,
        /// Whether that tree had uncommitted changes.
        pub dirty: bool,
        /// The tuner ruleset the simulated runs followed.
        pub ruleset: u32,
        /// The evidence epoch the simulated runs recorded under.
        pub evidence: u32,
        pub files: Vec<File>,
        pub runs: Vec<Run>,
        pub summary: Summary,
    }
}

impl Record {
    /// A forecast made by this build's tuner from `runs`, which must be non-empty and agree on their cores.
    pub fn new(
        anchor: Anchor,
        commit: impl Into<String>,
        dirty: bool,
        files: Vec<File>,
        runs: Vec<Run>,
    ) -> Result<Self, Error> {
        let summary = summarize(&runs)?;
        Ok(Self {
            anchor,
            commit: commit.into(),
            dirty,
            ruleset: RULESET,
            evidence: EVIDENCE_EPOCH,
            files,
            runs,
            summary,
        })
    }

    /// Encodes the record as indented JSON ending in a newline.
    pub fn write(&self, out: &mut impl Write) -> Result<(), Error> {
        serde_json::to_writer_pretty(&mut *out, self)
            .map_err(|source| Error::WriteRecord(source.into()))?;
        out.write_all(b"\n").map_err(Error::WriteRecord)
    }

    /// Decodes a committed forecast; one without runs is refused.
    pub fn read(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| Error::ReadRecord {
            path: path.to_path_buf(),
            source,
        })?;
        Self::decode_bytes(&bytes, path)
    }

    /// Decodes a record from a reader; `path` is only a diagnostic label.
    pub fn decode(mut reader: impl Read, path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|source| Error::ReadRecord {
                path: path.to_path_buf(),
                source,
            })?;
        Self::decode_bytes(&bytes, path)
    }

    fn decode_bytes(bytes: &[u8], path: &Path) -> Result<Self, Error> {
        let record: Self = serde_json::from_slice(bytes).map_err(|source| Error::DecodeRecord {
            path: path.to_path_buf(),
            source,
        })?;
        if record.runs.is_empty() {
            return Err(Error::EmptyRecord {
                path: path.to_path_buf(),
            });
        }
        Ok(record)
    }
}
