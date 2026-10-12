//! What happened, as structured entries: the lines of the history panel and the raw lines of the log view.

use std::borrow::Cow;

use jiff::Timestamp;
use togi_journal::{Inference, Seq};
use togi_machine::{CoreId, Signal, TargetOffset};

use crate::words::{Tag, Tone};

/// How many history entries are kept, newest first.
pub const HISTORY_LIMIT: usize = 200;
/// How many journal lines the log keeps, oldest first.
pub const LOG_LIMIT: usize = 400;

/// How an entry builds its sentence from what merged into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryKind {
    /// A sentence kept as it was written.
    #[default]
    Plain,
    /// Trials of one requirement or part: the subject, then how many passed.
    Trials,
    /// Solo limits found at one offset.
    Limits,
    /// Hunt groups answered by existing evidence.
    Groups,
    /// The session start, with the solo limits carried into it.
    Session,
}

/// What lets two neighbouring entries merge into one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// A solo limit.
    Limit,
    /// Passes of the trial named by the text.
    Pass(String),
    /// A skip of a trial that already failed at these offsets.
    KnownFailure,
    /// The start of a hunt.
    HuntStart,
    /// A checking step of a cycle.
    Step {
        /// The cycle.
        cycle: u32,
        /// The step, counting from 1.
        step: u32,
    },
    /// Hunt groups answered by carried trials.
    Carried {
        /// The hunt.
        hunt: u32,
        /// The answer.
        inferred: Inference,
    },
}

/// One line of what happened, in plain words or merged from several events.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// When it happened; a merged entry shows its last event.
    pub at: Timestamp,
    /// The tag column.
    pub tag: Tag,
    /// The sentence, with raw untrusted text in it.
    pub text: String,
    /// How the line is drawn.
    pub tone: Tone,
    /// How the sentence is built.
    pub kind: EntryKind,
    /// What entries merge on.
    pub key: Option<Key>,
    /// The words of `text` drawn as an alarm.
    pub alarm: Option<String>,
    /// Trials merged, for [`EntryKind::Trials`].
    pub runs: u32,
    /// The number of the first merged trial in its part or requirement.
    pub first: u32,
    /// How many trials its part or requirement needs.
    pub of: u32,
    /// The highest Tctl of the merged passes.
    pub peak: Option<i32>,
    /// The reboot this entry reports.
    pub reboot: Option<Seq>,
    /// The hunt it belongs to.
    pub hunt: Option<u32>,
    /// The first hunt group it covers.
    pub first_group: u32,
    /// The last hunt group it covers.
    pub last_group: u32,
    /// Whether these are a member probe's trials.
    pub probe: bool,
    /// The cores it names.
    pub cores: Vec<CoreId>,
    /// The offset of merged solo limits.
    pub offset: Option<TargetOffset>,
    /// Solo limits carried into the session, for [`EntryKind::Session`].
    pub count: u32,
    /// The answer of merged carried groups.
    pub inferred: Option<Inference>,
    /// The signal of a failure the sentence names.
    pub signal: Option<Signal>,
}

/// One raw line of the journal, as the log view lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    /// The event's sequence number.
    pub seq: Seq,
    /// When it happened.
    pub at: Timestamp,
    /// The tag column: a tag word, or the event kind with dots as spaces.
    pub tag: Cow<'static, str>,
    /// The recorded message, raw.
    pub msg: String,
}
