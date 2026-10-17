use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use togi_machine::object_only;
use togi_machine::serde_object::{self, Object};

use super::manifest::Pinned;
use crate::suite::hex;

/// One line of `experiments.jsonl`. The ledger is only ever appended to; it decodes from an object only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Record {
    CampaignStarted {
        at: Timestamp,
        manifest_sha256: String,
        head: String,
    },
    CampaignResumed {
        at: Timestamp,
        head: String,
    },
    ExperimentStarted(Started),
    ExperimentEnded(Ended),
    Outcome(Decision),
    CampaignStopped {
        at: Timestamp,
        reason: StopReason,
        detail: String,
    },
    /// An append found the ledger ending in a fragment a killed or failed write left, ended it with a newline
    /// and kept its bytes here, hex-encoded: the fragment is never a record.
    LedgerTorn {
        at: Timestamp,
        discarded: String,
    },
}

/// The evaluator a run invokes, through its `just` recipe.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Recipe {
    Forward,
    Fit,
    Bench,
}

impl Recipe {
    pub fn name(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Fit => "fit",
            Self::Bench => "bench",
        }
    }
}

/// What a run measures for its candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    /// The unchanged reference on the development data.
    Reference,
    /// The candidate on the development data.
    Candidate,
    /// The kept candidate on the confirmation data, once.
    Confirmation,
    /// The unchanged reference on the confirmation data, after the candidate's confirmation started.
    ConfirmationReference,
    /// A finalization check of a confirmed candidate.
    Finalization,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Candidate => "candidate",
            Self::Confirmation => "confirmation",
            Self::ConfirmationReference => "confirmation-reference",
            Self::Finalization => "finalization",
        }
    }
}

object_only! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Started {
        pub seq: u32,
        pub at: Timestamp,
        pub candidate: String,
        pub role: Role,
        pub recipe: Recipe,
        pub args: Vec<String>,
        pub head: String,
        /// The SHA-256 of every path that differs from the checkout, with its content: the candidate's identity.
        pub tree_sha256: String,
        pub reference: String,
        pub pins: Vec<Pinned>,
        pub stdout: String,
        pub stderr: String,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Ended {
        pub seq: u32,
        pub at: Timestamp,
        #[serde(deserialize_with = "serde_object::deserialize")]
        pub end: End,
        pub stdout_sha256: Option<String>,
        pub stderr_sha256: Option<String>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Decision {
        pub at: Timestamp,
        pub candidate: String,
        pub outcome: Outcome,
        pub reason: String,
        /// Full-precision metrics as the researcher read them from the runs' structured output.
        pub metrics: BTreeMap<String, f64>,
        /// The runs the outcome rests on, by sequence number.
        pub runs: Vec<u32>,
    }
}

/// How a run ended. Only `Exited` with code 0 is a complete run; every other end is incomplete.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum End {
    Exited {
        code: i32,
    },
    /// Killed by a signal the dispatcher did not send.
    Signalled,
    /// Stopped by the dispatcher at the campaign's wall-time limit.
    TimedOut,
    /// Stopped by the dispatcher when it was told to stop.
    Interrupted,
    /// Found unfinished when the campaign resumed.
    Incomplete,
    /// The evaluator could not be started or waited for.
    Unrunnable,
}

impl End {
    pub fn complete(self) -> bool {
        self == Self::Exited { code: 0 }
    }

    pub fn describe(self) -> String {
        match self {
            Self::Exited { code } => format!("exit {code}"),
            Self::Signalled => "killed by a signal".to_owned(),
            Self::TimedOut => "stopped at the wall-time limit".to_owned(),
            Self::Interrupted => "interrupted".to_owned(),
            Self::Unrunnable => "could not be run".to_owned(),
            Self::Incomplete => "incomplete: found unfinished on resume".to_owned(),
        }
    }
}

/// The scientific outcome of a candidate, decided by the brief's keep rule, never by a command's exit status.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Kept,
    Rejected,
    /// The experiment broke a rule of its program or brief and measures nothing.
    Invalid,
    ConfirmationPassed,
    ConfirmationFailed,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Kept => "kept",
            Self::Rejected => "rejected",
            Self::Invalid => "invalid",
            Self::ConfirmationPassed => "confirmation passed",
            Self::ConfirmationFailed => "confirmation failed",
        }
    }
}

/// Why a campaign stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    QueueExhausted,
    WallLimit,
    CandidateLimit,
    ConfirmationFailed,
    EvidenceChanged,
    ForbiddenDiff,
    UnpairedComparison,
    MissingPrerequisite,
    Interrupted,
}

impl StopReason {
    pub fn name(self) -> &'static str {
        match self {
            Self::QueueExhausted => "queue exhausted",
            Self::WallLimit => "wall-time limit reached",
            Self::CandidateLimit => "candidate limit reached",
            Self::ConfirmationFailed => "confirmation failed",
            Self::EvidenceChanged => "evidence or a frozen input changed",
            Self::ForbiddenDiff => "forbidden diff",
            Self::UnpairedComparison => "unpaired comparison",
            Self::MissingPrerequisite => "missing prerequisite",
            Self::Interrupted => "interrupted",
        }
    }
}

/// The ledger as read back: its records in order, with the questions every action asks of them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    pub records: Vec<Record>,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("decode ledger line {line}")]
    Decode {
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("the ledger does not start with campaign_started")]
    NoStart,
}

/// Decodes one complete ledger line, from a JSON object only.
fn decode(line: &[u8]) -> Result<Record, serde_json::Error> {
    serde_json::from_slice::<Object<Record>>(line).map(|Object(record)| record)
}

/// The bytes after the ledger's last newline: a fragment a killed or failed append left, if any.
fn torn_tail(bytes: &[u8]) -> Option<&[u8]> {
    let start = bytes
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |end| end + 1);
    Some(&bytes[start..]).filter(|tail| !tail.is_empty())
}

/// What an append writes before its records when the ledger ends in a torn fragment: a newline that ends the
/// fragment, then a `ledger_torn` record holding its bytes. Nothing for a ledger that ends cleanly.
pub(super) fn end_torn_tail(ledger: &[u8], now: Timestamp) -> Result<Vec<u8>, serde_json::Error> {
    let Some(tail) = torn_tail(ledger) else {
        return Ok(Vec::new());
    };
    let mut bytes = vec![b'\n'];
    serde_json::to_writer(
        &mut bytes,
        &Record::LedgerTorn {
            at: now,
            discarded: hex(tail),
        },
    )?;
    bytes.push(b'\n');
    Ok(bytes)
}

impl Ledger {
    /// Reads the ledger's complete lines. Bytes after the last newline are a torn tail, never a record, and a
    /// fragment directly followed by the `ledger_torn` record holding its bytes is skipped the same way.
    pub fn parse(bytes: &[u8]) -> Result<Self, LedgerError> {
        let complete = &bytes[..bytes.len() - torn_tail(bytes).map_or(0, <[u8]>::len)];
        let mut records = Vec::new();
        let mut lines = complete.split(|&byte| byte == b'\n').enumerate().peekable();
        while let Some((index, line)) = lines.next() {
            if line.is_empty() {
                continue;
            }
            let repair = lines
                .peek()
                .and_then(|(_, next)| decode(next).ok())
                .filter(|next| {
                    matches!(next, Record::LedgerTorn { discarded, .. } if *discarded == hex(line))
                });
            if let Some(repair) = repair {
                records.push(repair);
                lines.next();
                continue;
            }
            let record = decode(line).map_err(|source| LedgerError::Decode {
                line: index + 1,
                source,
            })?;
            records.push(record);
        }
        match records.first() {
            Some(Record::CampaignStarted { .. }) => Ok(Self { records }),
            _ => Err(LedgerError::NoStart),
        }
    }

    pub fn started_at(&self) -> Option<Timestamp> {
        self.records.iter().find_map(|record| match record {
            Record::CampaignStarted { at, .. } => Some(*at),
            _ => None,
        })
    }

    pub fn manifest_sha256(&self) -> Option<&str> {
        self.records.iter().find_map(|record| match record {
            Record::CampaignStarted {
                manifest_sha256, ..
            } => Some(manifest_sha256.as_str()),
            _ => None,
        })
    }

    pub fn stopped(&self) -> Option<(StopReason, &str)> {
        self.records.iter().find_map(|record| match record {
            Record::CampaignStopped { reason, detail, .. } => Some((*reason, detail.as_str())),
            _ => None,
        })
    }

    pub fn starts(&self) -> impl Iterator<Item = &Started> {
        self.records.iter().filter_map(|record| match record {
            Record::ExperimentStarted(started) => Some(started),
            _ => None,
        })
    }

    pub fn end(&self, seq: u32) -> Option<&Ended> {
        self.records.iter().find_map(|record| match record {
            Record::ExperimentEnded(ended) if ended.seq == seq => Some(ended),
            _ => None,
        })
    }

    pub fn decisions(&self) -> impl Iterator<Item = &Decision> {
        self.records.iter().filter_map(|record| match record {
            Record::Outcome(decision) => Some(decision),
            _ => None,
        })
    }

    /// Runs that started and never ended: the dispatcher died during them.
    pub fn dangling(&self) -> Vec<u32> {
        self.starts()
            .filter(|started| self.end(started.seq).is_none())
            .map(|started| started.seq)
            .collect()
    }

    pub fn next_seq(&self) -> u32 {
        self.starts()
            .map(|started| started.seq)
            .max()
            .map_or(1, |seq| seq + 1)
    }

    /// The distinct candidates that started a candidate run, in first-run order.
    pub fn candidates(&self) -> Vec<&str> {
        let mut candidates: Vec<&str> = Vec::new();
        for started in self.starts() {
            if started.role == Role::Candidate && !candidates.contains(&started.candidate.as_str())
            {
                candidates.push(&started.candidate);
            }
        }
        candidates
    }

    /// Confirmations started, whatever their end: a started confirmation is spent.
    pub fn confirmations(&self) -> Vec<&Started> {
        self.starts()
            .filter(|started| started.role == Role::Confirmation)
            .collect()
    }

    /// The candidate's latest outcome.
    pub fn outcome(&self, candidate: &str) -> Option<&Decision> {
        self.decisions()
            .filter(|decision| decision.candidate == candidate)
            .last()
    }

    /// The candidate's latest complete run in `role`.
    pub fn latest_complete(&self, candidate: &str, role: Role) -> Option<&Started> {
        self.starts()
            .filter(|started| {
                started.candidate == candidate
                    && started.role == role
                    && self
                        .end(started.seq)
                        .is_some_and(|ended| ended.end.complete())
            })
            .last()
    }

    /// The latest complete run in `role` with the same evaluator, arguments and pinned data as `run`.
    pub fn pair(&self, run: &Started, role: Role) -> Option<&Started> {
        self.starts()
            .filter(|started| {
                started.role == role
                    && started.recipe == run.recipe
                    && started.args == run.args
                    && started.pins == run.pins
                    && self
                        .end(started.seq)
                        .is_some_and(|ended| ended.end.complete())
            })
            .last()
    }
}
