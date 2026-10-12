use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use togi_journal::{DeadEndCondition, ShutdownReason};
use togi_tool_audit::Violation;

use crate::run::RunId;

/// How one run ended. Only `Clean` passes the suite: the session stopped after its clean cycles, the resume state
/// of its last boot matched a fresh fold of the journal, the tuner replays the journal and the audit found nothing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Clean,
    Failed {
        detail: String,
    },
    Capped {
        boots: u32,
    },
    DeadEnd {
        condition: DeadEndCondition,
        detail: String,
    },
    Stopped {
        reason: ShutdownReason,
    },
    VerifyFailed {
        detail: String,
    },
    ReplayFailed {
        detail: String,
    },
    AuditViolations {
        violations: Vec<Violation>,
    },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Clean => f.write_str("clean"),
            Self::Failed { detail } => write!(f, "failed: {detail}"),
            Self::Capped { boots } => {
                write!(f, "reached the cap of {boots} boots without stopping")
            }
            Self::DeadEnd { condition, detail } => write!(f, "dead end {condition}: {detail}"),
            Self::Stopped { reason } => {
                write!(f, "stopped for {reason} instead of completing its cycles")
            }
            Self::VerifyFailed { detail } => {
                write!(f, "warm state differs from a fresh replay: {detail}")
            }
            Self::ReplayFailed { detail } => write!(f, "journal does not replay: {detail}"),
            Self::AuditViolations { violations } => {
                write!(f, "{} audit violations", violations.len())?;
                for violation in violations {
                    write!(f, "; {violation}")?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunResult {
    pub id: RunId,
    pub outcome: Outcome,
}

#[derive(Debug, thiserror::Error)]
#[error("manifest line {line}: {source}")]
pub struct ManifestError {
    pub line: usize,
    #[source]
    pub source: serde_json::Error,
}

/// Reads a shard's manifest: one JSON object per line, one result per run.
pub fn parse_manifest(text: &str) -> Result<Vec<RunResult>, ManifestError> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| ManifestError {
                line: index + 1,
                source,
            })
        })
        .collect()
}

pub fn encode_manifest(results: &[RunResult]) -> Result<String, serde_json::Error> {
    let mut text = String::new();
    for result in results {
        text.push_str(&serde_json::to_string(result)?);
        text.push('\n');
    }
    Ok(text)
}

/// The results one shard reported, tagged with the shard index its manifest was produced for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShardManifest {
    pub shard: usize,
    pub results: Vec<RunResult>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Problem {
    #[error("shard {shard} has no manifest")]
    MissingShard { shard: usize },
    #[error("shard {shard} has a manifest but is not one of the expected shards")]
    UnexpectedShard { shard: usize },
    #[error("shard {shard} has more than one manifest")]
    DuplicateShard { shard: usize },
    #[error("run {id} is in no manifest")]
    MissingRun { id: RunId },
    #[error("run {id} is reported {} times, by shards {shards:?}", shards.len())]
    DuplicateRun { id: RunId, shards: Vec<usize> },
    #[error("shard {shard} reports run {id}, which is not one of the runs checked")]
    UnexpectedRun { id: RunId, shard: usize },
    #[error("run {id} in shard {shard}: {outcome}")]
    Unclean {
        id: RunId,
        shard: usize,
        outcome: Outcome,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub struct CoverageError {
    pub problems: Vec<Problem>,
}

impl fmt::Display for CoverageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} suite problems", self.problems.len())?;
        for problem in &self.problems {
            write!(f, "\n  {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CoverageError {}

/// Proves the suite ran completely and cleanly. `expected` is the run ids and `expected_shards` the shards that ran
/// them, both derived from the suite and its partition without looking at the manifests, so a shard whose manifest
/// is absent is a problem rather than invisible: `0..count` for the whole suite, one index for a single shard's own
/// check. Every missing shard, missing run, duplicate, unexpected id and unclean outcome is reported, not only the
/// first.
pub fn check(
    expected: &[RunId],
    expected_shards: impl IntoIterator<Item = usize>,
    manifests: &[ShardManifest],
) -> Result<(), CoverageError> {
    let mut problems = Vec::new();
    let mut manifested: BTreeMap<usize, usize> = expected_shards
        .into_iter()
        .map(|shard| (shard, 0))
        .collect();
    for manifest in manifests {
        match manifested.get_mut(&manifest.shard) {
            Some(count) => *count += 1,
            None => problems.push(Problem::UnexpectedShard {
                shard: manifest.shard,
            }),
        }
    }
    for (&shard, &count) in &manifested {
        match count {
            0 => problems.push(Problem::MissingShard { shard }),
            1 => {}
            _ => problems.push(Problem::DuplicateShard { shard }),
        }
    }
    let mut reported: BTreeMap<&RunId, Vec<usize>> = BTreeMap::new();
    for manifest in manifests {
        for result in &manifest.results {
            reported.entry(&result.id).or_default().push(manifest.shard);
        }
    }
    let known: BTreeSet<&RunId> = expected.iter().collect();
    for id in expected {
        if !reported.contains_key(id) {
            problems.push(Problem::MissingRun { id: id.clone() });
        }
    }
    for (id, shards) in &reported {
        if shards.len() > 1 {
            problems.push(Problem::DuplicateRun {
                id: (*id).clone(),
                shards: shards.clone(),
            });
        }
        if !known.contains(id) {
            for &shard in shards {
                problems.push(Problem::UnexpectedRun {
                    id: (*id).clone(),
                    shard,
                });
            }
        }
    }
    for manifest in manifests {
        for result in &manifest.results {
            if result.outcome != Outcome::Clean {
                problems.push(Problem::Unclean {
                    id: result.id.clone(),
                    shard: manifest.shard,
                    outcome: result.outcome.clone(),
                });
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(CoverageError { problems })
    }
}
