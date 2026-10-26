//! What happened, as structured entries: the lines of the history panel and the raw lines of the log view.

use std::borrow::Cow;
use std::fmt::Write as _;

use jiff::Timestamp;
use togi_journal::Seq;
use togi_machine::{CoreId, Signal};

use crate::words::{Tag, Tone, signal_words};

/// How many history entries are kept, newest first.
pub const HISTORY_LIMIT: usize = 200;
/// How many journal lines the log keeps, oldest first.
pub const LOG_LIMIT: usize = 400;

/// Passed trials of one subject, merged while they follow one another.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Passes {
    /// What the trials tested and loaded, raw untrusted text in it; passes merge only with passes of the same
    /// subject.
    pub subject: String,
    /// Trials merged.
    pub runs: u32,
    /// The highest Tctl of the merged passes.
    pub peak: Option<i32>,
}

impl Passes {
    fn sentence(&self) -> String {
        let mut text = if self.runs > 1 {
            format!("{} · {} passed", self.subject, self.runs)
        } else {
            format!("{} · passed", self.subject)
        };
        if let Some(peak) = self.peak {
            let _ = write!(text, " · Tctl max {peak}°C");
        }
        text
    }
}

/// Whom a failed trial's evidence names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Named {
    /// The trial names this core.
    Core(CoreId),
}

impl Named {
    /// "core 04 named".
    pub fn words(self) -> String {
        match self {
            Self::Core(core) => format!("core {:02} named", core.get()),
        }
    }
}

/// How a trial ended, kept in parts so the history panel can reserve what attributes the failure before it fits
/// the context. The journal's own message and the log view never pass through here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Verdict {
    /// The signal the trial ended with, when the journal names one.
    pub signal: Option<Signal>,
    /// How far into the trial it got, when known: "early in trial 2 of 5". Context, the first thing a narrow
    /// panel gives up.
    pub when: Option<String>,
    /// The attribution. Decisive, so a narrow panel keeps it.
    pub named: Option<Named>,
}

impl Verdict {
    /// What the trial did: "rebooted", "wrong result" and the like.
    pub fn signal_words(&self) -> &'static str {
        match self.signal {
            Some(Signal::Crash) => "rebooted",
            Some(signal) => signal_words(signal),
            None => "failed",
        }
    }

    /// Whether the signal is drawn as an alarm: every failure but a reboot, which its tag already reports.
    pub fn alarm(&self) -> bool {
        self.signal != Some(Signal::Crash)
    }

    /// The parts after the signal, in order.
    fn context(&self) -> impl Iterator<Item = String> {
        self.when
            .clone()
            .into_iter()
            .chain(self.named.map(Named::words))
    }
}

/// A failed trial.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Failed {
    /// What the trial loaded, raw untrusted text in it.
    pub subject: String,
    /// How it ended.
    pub verdict: Verdict,
    /// The reboot it reports, when it replaced that reboot's line.
    pub reboot: Option<Seq>,
}

/// What an entry says, with the fields its sentence is built from.
#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum What {
    /// A sentence kept as it was written, raw untrusted text in it.
    Said(String),
    /// The session start.
    Session {
        /// The cores the session runs on.
        cores: u32,
    },
    /// Passed trials, merged.
    Passes(Passes),
    /// A failed trial.
    Failed(Failed),
    /// A reboot with the offsets applied, until the trial it ended replaces it.
    Reboot {
        /// Whether no trial was running.
        idle: bool,
        /// The event that recorded it.
        reboot: Seq,
    },
}

/// One line of what happened, in plain words or merged from several events.
#[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    /// When it happened; a merged entry shows its last event.
    pub at: Timestamp,
    /// The tag column.
    pub tag: Tag,
    /// How the line is drawn.
    pub tone: Tone,
    /// What it says.
    pub what: What,
}

/// One raw line of the journal, as the log view lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

impl Entry {
    /// A line with a tag, a tone and what it says.
    pub const fn new(at: Timestamp, tag: Tag, what: What, tone: Tone) -> Self {
        Self {
            at,
            tag,
            tone,
            what,
        }
    }

    /// A plain line: a tag, a sentence and a tone, merging with nothing.
    pub const fn line(at: Timestamp, tag: Tag, text: String, tone: Tone) -> Self {
        Self::new(at, tag, What::Said(text), tone)
    }

    /// The reboot the line reports.
    pub fn reboot(&self) -> Option<Seq> {
        match &self.what {
            What::Reboot { reboot, .. } => Some(*reboot),
            What::Failed(failed) => failed.reboot,
            What::Said(_) | What::Session { .. } | What::Passes(_) => None,
        }
    }

    /// The whole sentence as the plain-text consumers read it.
    pub fn text(&self) -> String {
        match &self.what {
            What::Said(text) => text.clone(),
            What::Session { cores } => format!("session started on {cores} cores"),
            What::Passes(passes) => passes.sentence(),
            What::Failed(failed) => {
                let mut text = format!("{} · {}", failed.subject, failed.verdict.signal_words());
                for part in failed.verdict.context() {
                    let _ = write!(text, " · {part}");
                }
                text
            }
            What::Reboot { idle: true, .. } => "idle with the offsets applied, rebooted".to_owned(),
            What::Reboot { idle: false, .. } => "with the offsets applied, rebooted".to_owned(),
        }
    }

    /// The sentence split around the words drawn as an alarm: the words before, the alarm and the words after.
    /// Without an alarm everything is before.
    pub fn sentence(&self) -> (String, String, String) {
        let sentence = self.sentence_words();
        #[cfg(test)]
        crate::corpus::sentence(self, &sentence);
        sentence
    }

    fn sentence_words(&self) -> (String, String, String) {
        if let What::Failed(failed) = &self.what
            && failed.verdict.alarm()
        {
            let mut after = String::new();
            for part in failed.verdict.context() {
                let _ = write!(after, " · {part}");
            }
            return (
                format!("{} · ", failed.subject),
                failed.verdict.signal_words().to_owned(),
                after,
            );
        }
        (self.text(), String::new(), String::new())
    }

    /// Merges `line` into this entry when it continues it: passes of the same subject.
    fn absorb(&mut self, line: &Self) -> bool {
        match (&mut self.what, &line.what) {
            (What::Passes(kept), What::Passes(next)) if kept.subject == next.subject => {
                kept.runs += next.runs;
                kept.peak = kept.peak.max(next.peak);
            }
            _ => return false,
        }
        self.at = line.at;
        true
    }
}

/// Appends a line, merging it into the line before when it continues it.
pub(crate) fn fold_entry(history: &mut Vec<Entry>, line: Entry) {
    if let Some(last) = history.last_mut()
        && last.absorb(&line)
    {
        return;
    }
    history.push(line);
}

/// The kept history: entries in the order they happened, and how many older ones were let go.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct History {
    pub(crate) entries: Vec<Entry>,
    pub(crate) dropped: usize,
    pub(crate) limit: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            dropped: 0,
            limit: HISTORY_LIMIT,
        }
    }
}

impl History {
    /// Appends a line, merging it into the one before, and lets the oldest go once twice the limit is kept.
    pub(crate) fn push(&mut self, line: Entry) {
        fold_entry(&mut self.entries, line);
        if self.entries.len() > 2 * self.limit {
            let drop = self.entries.len() - self.limit;
            self.dropped += drop;
            self.entries.drain(..drop);
        }
    }

    /// The history to show: the newest [`HISTORY_LIMIT`] kept, newest first, and how many older ones were let go.
    pub(crate) fn recent(&self) -> (Vec<Entry>, usize) {
        let mut kept = self.entries.clone();
        let mut dropped = self.dropped;
        if kept.len() > self.limit {
            let drop = kept.len() - self.limit;
            dropped += drop;
            kept.drain(..drop);
        }
        kept.reverse();
        (kept, dropped)
    }
}

#[cfg(test)]
mod corpus_form {
    use super::{Entry, What};

    impl Entry {
        /// The name of the kind of sentence, as the narration corpus files it.
        pub(crate) const fn form(&self) -> &'static str {
            match self.what {
                What::Said(_) => "Said",
                What::Session { .. } => "Session",
                What::Passes(_) => "Passes",
                What::Failed(_) => "Failed",
                What::Reboot { .. } => "Reboot",
            }
        }
    }
}
