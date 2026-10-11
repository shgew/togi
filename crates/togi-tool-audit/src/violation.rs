use std::fmt;

use serde::{Deserialize, Serialize};
use togi_journal::{Seq, SessionId};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    Sequence,
    Cause,
    Avoidance,
    WriteProtocol,
    Termination,
    Replay,
}

impl Check {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sequence => "sequence",
            Self::Cause => "cause",
            Self::Avoidance => "avoidance",
            Self::WriteProtocol => "write_protocol",
            Self::Termination => "termination",
            Self::Replay => "replay",
        }
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Violation {
    pub seq: Seq,
    pub check: Check,
    pub reason: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "seq {} {}: {}", self.seq, self.check, self.reason)
    }
}

/// Whether the audited journal ended its run. Only a finished run must end in a conclusion and carry no write
/// without its readback; an open or archived journal may end at a transition or an interrupted action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    InProgress,
    Finished,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    Archived(SessionId),
    Live,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalAudit {
    pub source: Source,
    pub violations: Vec<Violation>,
}

#[derive(Debug, Default)]
pub(crate) struct Findings(Vec<Violation>);

impl Findings {
    pub(crate) fn add(&mut self, seq: Seq, check: Check, reason: impl Into<String>) {
        self.0.push(Violation {
            seq,
            check,
            reason: reason.into(),
        });
    }

    pub(crate) fn into_vec(self) -> Vec<Violation> {
        self.0
    }
}
