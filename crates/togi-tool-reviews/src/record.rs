//! The hidden JSON block that ends a review record comment (docs/review-record.md).

use std::fmt;

use serde::Deserialize;
use serde_json::value::RawValue;

use crate::text::{nullable, quote};

pub const RECORD_OPEN: &str = "<!-- togi-review ";
pub const RECORD_CLOSE: &str = " -->";
pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];
pub const OUTCOMES: [&str; 3] = ["fixed", "rejected", "deferred"];

/// The part of a record's block this tool reads; versions 1, 2 and 3 share it.
#[derive(Debug, Default, Deserialize)]
pub struct Record {
    #[serde(default, deserialize_with = "nullable")]
    pub version: i64,
    /// In every version.
    #[serde(default, deserialize_with = "nullable")]
    pub head_sha: String,
    /// From version 2.
    #[serde(default, deserialize_with = "nullable")]
    pub base_sha: String,
    #[serde(default, deserialize_with = "nullable")]
    pub findings: Vec<Finding>,
    /// In every version 3 record.
    #[serde(default, deserialize_with = "nullable")]
    pub verdict: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct Finding {
    #[serde(default, deserialize_with = "nullable")]
    pub source: String,
    #[serde(default, deserialize_with = "nullable")]
    pub priority: String,
    #[serde(default, deserialize_with = "nullable")]
    pub finding: String,
    /// Kept raw: version 1 records it as an object and later versions as a string.
    #[serde(default)]
    pub location: Option<Box<RawValue>>,
    #[serde(default, deserialize_with = "nullable")]
    pub outcome: Outcome,
}

impl Finding {
    /// The location's JSON, `null` when absent.
    pub fn location(&self) -> &str {
        self.location.as_deref().map_or("null", RawValue::get)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Outcome {
    #[serde(default, deserialize_with = "nullable")]
    pub status: String,
    #[serde(default, deserialize_with = "nullable")]
    pub sha: String,
    #[serde(default, deserialize_with = "nullable")]
    pub reason: String,
    #[serde(default, deserialize_with = "nullable")]
    pub issue: i64,
}

#[derive(Debug)]
pub enum RecordError {
    NoBlock,
    Unterminated,
    Decode(serde_json::Error),
    Version(i64),
    Priority { source: String, priority: String },
    Outcome { source: String, status: String },
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBlock => f.write_str("no togi-review block"),
            Self::Unterminated => f.write_str("unterminated togi-review block"),
            Self::Decode(error) => write!(f, "decode togi-review block: {error}"),
            Self::Version(version) => write!(f, "unsupported record version {version}"),
            Self::Priority { source, priority } => {
                write!(
                    f,
                    "finding from {source}: unknown priority {}",
                    quote(priority)
                )
            }
            Self::Outcome { source, status } => {
                write!(
                    f,
                    "finding from {source}: unknown outcome {}",
                    quote(status)
                )
            }
        }
    }
}

impl std::error::Error for RecordError {}

/// Reads the hidden block that ends a review record comment.
pub fn parse_record(body: &str) -> Result<Record, RecordError> {
    let Some(start) = body.rfind(RECORD_OPEN) else {
        return Err(RecordError::NoBlock);
    };
    let rest = &body[start + RECORD_OPEN.len()..];
    let Some(end) = rest.find(RECORD_CLOSE) else {
        return Err(RecordError::Unterminated);
    };
    let record: Record = serde_json::from_str(&rest[..end]).map_err(RecordError::Decode)?;
    if !(1..=3).contains(&record.version) {
        return Err(RecordError::Version(record.version));
    }
    for finding in &record.findings {
        if !PRIORITIES.contains(&finding.priority.as_str()) {
            return Err(RecordError::Priority {
                source: finding.source.clone(),
                priority: finding.priority.clone(),
            });
        }
        if !OUTCOMES.contains(&finding.outcome.status.as_str()) {
            return Err(RecordError::Outcome {
                source: finding.source.clone(),
                status: finding.outcome.status.clone(),
            });
        }
    }
    Ok(record)
}

#[cfg(test)]
#[path = "tests/record.rs"]
pub(crate) mod tests;
