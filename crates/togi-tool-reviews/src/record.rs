//! The hidden JSON block that ends a review record comment (docs/review-record.md).

use serde::Serialize;
use serde_json::value::RawValue;
use togi_machine::object_only;

use crate::text::{nullable, quote};

pub const RECORD_OPEN: &str = "<!-- togi-review ";
pub const RECORD_CLOSE: &str = " -->";
pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];
pub const OUTCOMES: [&str; 3] = ["fixed", "rejected", "deferred"];

object_only! {
    /// The part of a record's block this tool reads; versions 1, 2 and 3 share it.
    #[derive(Debug, Default, Serialize)]
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

    #[derive(Debug, Default, Serialize)]
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

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
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
}

impl Finding {
    /// The location's JSON, `null` when absent.
    pub fn location(&self) -> &str {
        self.location.as_deref().map_or("null", RawValue::get)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("no togi-review block")]
    NoBlock,
    #[error("unterminated togi-review block")]
    Unterminated,
    #[error("decode togi-review block")]
    Decode(#[source] serde_json::Error),
    #[error("unsupported record version {0}")]
    Version(i64),
    #[error("finding from {from}: unknown priority {}", quote(.priority))]
    Priority { from: String, priority: String },
    #[error("finding from {from}: unknown outcome {}", quote(.status))]
    Outcome { from: String, status: String },
}

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
                from: finding.source.clone(),
                priority: finding.priority.clone(),
            });
        }
        if !OUTCOMES.contains(&finding.outcome.status.as_str()) {
            return Err(RecordError::Outcome {
                from: finding.source.clone(),
                status: finding.outcome.status.clone(),
            });
        }
    }
    Ok(record)
}

#[cfg(test)]
#[path = "tests/record.rs"]
pub(crate) mod tests;
