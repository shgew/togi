//! What happened, as structured entries: the lines of the history panel and the raw lines of the log view.

use std::borrow::Cow;
use std::fmt::Write as _;

use jiff::Timestamp;
use togi_journal::{Inference, Seq};
use togi_machine::{CoreId, TargetOffset};

use crate::words::{Tag, Tone, core_ids, plural};

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

/// How a trial ended, kept in parts so the history panel can reserve what attributes the failure before it fits
/// the context. The journal's own message and the log view never pass through here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// What the trial did: "rebooted", "wrong result" and the like.
    pub signal: String,
    /// How far into the trial it got, when known: "early in trial 2 of 5". Context, the first thing a narrow
    /// panel gives up.
    pub when: Option<String>,
    /// The attribution: "core 04 named" or "no core named". Decisive, so a narrow panel keeps it.
    pub named: Option<String>,
}

/// One line of what happened, in plain words or merged from several events.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// When it happened; a merged entry shows its last event.
    pub at: Timestamp,
    /// The tag column.
    pub tag: Tag,
    /// The subject of a failure whose `verdict` follows it, or the whole sentence of any other entry; raw
    /// untrusted text in it.
    pub text: String,
    /// How the line is drawn.
    pub tone: Tone,
    /// How the sentence is built.
    pub kind: EntryKind,
    /// What entries merge on.
    pub key: Option<Key>,
    /// The words of `text` drawn as an alarm.
    pub alarm: Option<String>,
    /// How a failed trial ended, for the parts that follow `text`.
    pub verdict: Option<Verdict>,
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

impl Entry {
    /// A plain line: a tag, a sentence and a tone, merging with nothing.
    pub fn line(at: Timestamp, tag: Tag, text: String, tone: Tone) -> Self {
        Self {
            at,
            tag,
            text,
            tone,
            kind: EntryKind::Plain,
            key: None,
            alarm: None,
            verdict: None,
            runs: 0,
            first: 0,
            of: 0,
            peak: None,
            reboot: None,
            hunt: None,
            first_group: 0,
            last_group: 0,
            probe: false,
            cores: Vec::new(),
            offset: None,
            count: 0,
            inferred: None,
        }
    }

    /// The whole sentence as the plain-text consumers read it: `text`, then the parts of a `verdict`.
    pub fn full_text(&self) -> Cow<'_, str> {
        let Some(verdict) = &self.verdict else {
            return Cow::Borrowed(&self.text);
        };
        let mut text = format!("{} · {}", self.text, verdict.signal);
        for part in [&verdict.when, &verdict.named].into_iter().flatten() {
            let _ = write!(text, " · {part}");
        }
        Cow::Owned(text)
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
        match self.kind {
            EntryKind::Session => {
                let text = if self.count > 0 {
                    format!(
                        "session started · {} carried from the last session",
                        plural(self.count as usize, "solo limit")
                    )
                } else {
                    format!("session started on {} cores", self.of)
                };
                (text, String::new(), String::new())
            }
            EntryKind::Limits => {
                let offset = self.offset.map_or(0, TargetOffset::get);
                let mut cores = self.cores.clone();
                cores.sort_unstable();
                let mut text = match cores.as_slice() {
                    [core] => format!("core {:02} solo limit {offset}", core.get()),
                    _ => format!(
                        "cores {} solo limit {offset}",
                        cores
                            .iter()
                            .map(|core| format!("{:02}", core.get()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                if offset == -50 {
                    text.push_str(", the deepest offset");
                }
                (text, String::new(), String::new())
            }
            EntryKind::Groups => {
                let many = self.last_group > self.first_group;
                let groups = if many {
                    format!("groups {}-{}", self.first_group, self.last_group)
                } else {
                    format!("group {}", self.first_group)
                };
                let cores = core_ids(self.cores.iter().copied());
                let (what, verdict) = if self.inferred == Some(Inference::Failure) {
                    (cores, "failed on existing evidence")
                } else if many {
                    (
                        format!("parts of {cores}"),
                        "all passed on existing evidence",
                    )
                } else {
                    (cores, "passed on existing evidence")
                };
                (
                    format!(
                        "hunt {} {groups} · {what} · {verdict}",
                        self.hunt.unwrap_or(0)
                    ),
                    String::new(),
                    String::new(),
                )
            }
            EntryKind::Trials => {
                if self.probe && self.last_group > self.first_group {
                    return (
                        format!(
                            "hunt {} groups {}-{} · member probes · each {} passed",
                            self.hunt.unwrap_or(0),
                            self.first_group,
                            self.last_group,
                            self.tally()
                        ),
                        String::new(),
                        String::new(),
                    );
                }
                let mut text = if self.of > 1 || self.runs > 1 {
                    format!("{} · {} passed", self.text, self.tally())
                } else {
                    format!("{} · passed", self.text)
                };
                if let Some(peak) = self.peak {
                    let _ = write!(text, " · Tctl max {peak}°C");
                }
                (text, String::new(), String::new())
            }
            EntryKind::Plain => {
                let full = self.full_text();
                match self.alarm.as_deref() {
                    Some(alarm) if !alarm.is_empty() => match full.split_once(alarm) {
                        Some((before, after)) => {
                            (before.to_owned(), alarm.to_owned(), after.to_owned())
                        }
                        None => (full.into_owned(), String::new(), String::new()),
                    },
                    _ => (full.into_owned(), String::new(), String::new()),
                }
            }
        }
    }

    /// Merged trials counted against what their part or requirement needs: "4 of 4", "trials 1-3 of 4". Passes
    /// that run past the requirement, as when a failure leaves earlier passes standing, are only counted.
    pub fn tally(&self) -> String {
        let (runs, first, of) = (self.runs, self.first, self.of);
        if of == 0 || (of == 1 && runs == 1) || first + runs - 1 > of {
            runs.to_string()
        } else if first == 1 && runs == of {
            format!("{runs} of {of}")
        } else if runs == 1 {
            format!("trial {first} of {of}")
        } else {
            format!("trials {first}-{} of {of}", first + runs - 1)
        }
    }
}

/// Appends a line, merging it into the line before when both carry the same key.
pub(crate) fn fold_entry(history: &mut Vec<Entry>, line: Entry) {
    let Some(last) = history.last_mut() else {
        history.push(line);
        return;
    };
    if line.key.is_none() || last.key != line.key {
        history.push(line);
        return;
    }
    match line.kind {
        EntryKind::Limits => {
            if last.offset != line.offset {
                history.push(line);
                return;
            }
            last.cores.extend(line.cores.iter().copied());
        }
        EntryKind::Groups => {
            if last.last_group + 1 != line.first_group {
                history.push(line);
                return;
            }
            last.last_group = line.last_group;
            for core in &line.cores {
                if !last.cores.contains(core) {
                    last.cores.push(*core);
                }
            }
        }
        EntryKind::Trials => {
            last.runs += line.runs;
            last.of = last.of.max(line.of);
            if line.peak.is_some() && (last.peak.is_none() || line.peak > last.peak) {
                last.peak = line.peak;
            }
        }
        EntryKind::Plain | EntryKind::Session => {
            history.push(line);
            return;
        }
    }
    last.at = line.at;
}

/// Whether `line` is a member probe that passed every trial it needed, straight after one that did the same for
/// the hunt's previous group.
pub(crate) fn mergeable_probe_passes(last: &Entry, line: &Entry) -> bool {
    line.probe
        && line.tag == Tag::Pass
        && line.of > 0
        && line.runs == line.of
        && line.first == 1
        && last.probe
        && last.tag == Tag::Pass
        && last.hunt == line.hunt
        && last.last_group + 1 == line.first_group
        && last.of == line.of
        && last.runs == last.of
        && last.first == 1
}

/// Folds adjacent member probes that each passed every trial they needed into one line.
pub(crate) fn merge_probe_passes(history: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::with_capacity(history.len());
    for entry in history {
        if let Some(last) = out.last_mut() {
            if mergeable_probe_passes(last, &entry) {
                last.at = entry.at;
                last.last_group = entry.last_group;
                if entry.peak.is_some() && (last.peak.is_none() || entry.peak > last.peak) {
                    last.peak = entry.peak;
                }
                continue;
            }
        }
        out.push(entry);
    }
    out
}

/// The kept history: entries in the order they happened, and how many older ones were let go.
#[derive(Debug, Clone)]
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
    /// A history that keeps every entry, to compare the retained one against in tests.
    #[cfg(test)]
    pub(crate) fn unbounded() -> Self {
        Self {
            limit: usize::MAX / 4,
            ..Self::default()
        }
    }

    /// Appends a line, merging it into the one before, and lets the oldest go once twice the limit is kept.
    pub(crate) fn push(&mut self, line: Entry) {
        fold_entry(&mut self.entries, line);
        if self.entries.len() > 2 * self.limit {
            self.compact();
        }
    }

    fn compact(&mut self) {
        let last = self.entries.split_off(self.entries.len().saturating_sub(1));
        let mut kept = merge_probe_passes(std::mem::take(&mut self.entries));
        kept.extend(last);
        let mut drop = kept.len().saturating_sub(self.limit);
        let n = kept.len();
        if n > 1 && mergeable_probe_passes(&kept[n - 2], &kept[n - 1]) {
            drop = drop.saturating_sub(1);
        }
        if drop > 0 {
            self.dropped += drop;
            kept.drain(..drop);
        }
        self.entries = kept;
    }

    /// The history to show: probe passes merged across the retention boundary, the newest
    /// [`HISTORY_LIMIT`] kept, newest first, and how many older ones were let go.
    pub(crate) fn recent(&self) -> (Vec<Entry>, usize) {
        let mut kept = merge_probe_passes(self.entries.clone());
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
