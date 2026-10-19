//! What happened, as structured entries: the lines of the history panel and the raw lines of the log view.

use std::borrow::Cow;
use std::fmt::Write as _;

use jiff::Timestamp;
use togi_journal::{Inference, Seq};
use togi_machine::{CoreId, CoreSet, Regime, Signal, TargetOffset, WorkloadId};

use crate::describe::part_layout;
use crate::words::{Tag, Tone, core_ids, kind_words, plural, signal_words, workload_display};

/// How many history entries are kept, newest first.
pub const HISTORY_LIMIT: usize = 200;
/// How many journal lines the log keeps, oldest first.
pub const LOG_LIMIT: usize = 400;

/// The checking step a line belongs to: its place, the regime the cycle's plan gives it when known, and the workload
/// its trials run once one names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepAt {
    /// The cycle.
    pub cycle: u32,
    /// The step, counting from 1.
    pub step: u32,
    /// The step's regime, when the cycle's plan was known.
    pub regime: Option<Regime>,
    /// The workload the step's trials run, once a trial of the step names it.
    pub workload: Option<WorkloadId>,
}

impl StepAt {
    /// The step as its marker names it: `cycle 1 step 2: R2 heavy vector, y-cruncher FFTv4`.
    pub fn label(&self) -> String {
        let mut text = format!("cycle {} step {}", self.cycle, self.step);
        if let Some(regime) = self.regime {
            let _ = write!(text, ": {regime} {}", kind_words(regime));
        }
        if let Some(workload) = self.workload {
            let _ = write!(text, ", {}", workload_display(workload));
        }
        text
    }
}

/// A run of neighbouring groups of one hunt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Groups {
    /// The hunt.
    pub hunt: u32,
    /// The first group.
    pub first: u32,
    /// The last group.
    pub last: u32,
}

impl Groups {
    /// One group of a hunt.
    pub const fn one(hunt: u32, group: u32) -> Self {
        Self {
            hunt,
            first: group,
            last: group,
        }
    }

    /// Whether `next` starts straight after these groups end, in the same hunt.
    const fn continued_by(&self, next: &Self) -> bool {
        self.hunt == next.hunt && self.last + 1 == next.first
    }
}

/// Where passed trials ran, when it decides what they merge with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PassedIn {
    /// A checking step outside any hunt; passes of different steps never merge.
    Step(StepAt),
    /// Groups of a hunt.
    Hunt {
        /// The groups.
        groups: Groups,
        /// Whether these are a member probe's trials.
        probe: bool,
    },
}

/// Passed trials of one requirement or part.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Passes {
    /// What the trials loaded, raw untrusted text in it; passes merge only with passes of the same subject.
    pub subject: String,
    /// Trials merged.
    pub runs: u32,
    /// The number of the first merged trial in its part or requirement.
    pub first: u32,
    /// How many trials its part or requirement needs; 0 when unknown.
    pub of: u32,
    /// The highest Tctl of the merged passes.
    pub peak: Option<i32>,
    /// Where they ran.
    pub within: Option<PassedIn>,
}

impl Passes {
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

    /// Whether these passes are every trial their part or requirement needs, from the first.
    const fn complete(&self) -> bool {
        self.of > 0 && self.runs == self.of && self.first == 1
    }

    /// The checking step they ran in.
    const fn step(&self) -> Option<&StepAt> {
        match &self.within {
            Some(PassedIn::Step(at)) => Some(at),
            Some(PassedIn::Hunt { .. }) | None => None,
        }
    }

    /// The member probe's hunt groups these passes ran in, when they are a probe's.
    const fn probe_groups(&self) -> Option<&Groups> {
        match &self.within {
            Some(PassedIn::Hunt {
                groups,
                probe: true,
            }) => Some(groups),
            Some(PassedIn::Hunt { probe: false, .. } | PassedIn::Step(_)) | None => None,
        }
    }

    fn sentence(&self) -> String {
        if let Some(groups) = self.probe_groups()
            && groups.last > groups.first
        {
            return format!(
                "hunt {} groups {}-{} · member probes · each {} passed",
                groups.hunt,
                groups.first,
                groups.last,
                self.tally()
            );
        }
        let mut text = if self.of > 1 || self.runs > 1 {
            format!("{} · {} passed", self.subject, self.tally())
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
    /// A multi-core trial outside a hunt that names no core.
    Nobody,
}

impl Named {
    /// "core 04 named" or "no core named".
    pub fn words(self) -> String {
        match self {
            Self::Core(core) => format!("core {:02} named", core.get()),
            Self::Nobody => "no core named".to_owned(),
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
    /// The hunt it ran in.
    pub hunt: Option<u32>,
    /// The reboot it reports, when it replaced that reboot's line.
    pub reboot: Option<Seq>,
}

/// The hunt a known failure was answered with, and the cores it hunts among.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Located {
    /// The hunt.
    pub hunt: u32,
    /// The cores the failure loaded.
    pub cores: CoreSet,
}

/// A trial skipped because it already failed at these offsets.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct KnownFailure {
    /// The regime of the failed trial, when the journal names it.
    pub regime: Option<Regime>,
    /// Whether the earlier trial crashed rather than failed.
    pub crashed: bool,
    /// Whether the earlier trial was carried from the last session.
    pub carried: bool,
    /// The hunt the failure starts, once it does.
    pub hunt: Option<Located>,
}

impl KnownFailure {
    fn sentence(&self) -> String {
        let mut text = self.regime.map_or_else(
            || "a trial".to_owned(),
            |regime| format!("{regime} {}", kind_words(regime)),
        );
        if let Some(located) = self.hunt {
            let _ = write!(text, " on {}", core_ids(located.cores));
        }
        let verb = if self.crashed { "crashed" } else { "failed" };
        let _ = write!(text, " at these offsets already {verb}");
        if self.carried {
            text.push_str(" in a carried trial");
        }
        if let Some(located) = self.hunt {
            let _ = write!(text, " → hunt {}", located.hunt);
        }
        text
    }
}

/// How a hunt's first group loads its candidates, once the hunt names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FirstGroup {
    /// The candidates are located at offset 0.
    Locate,
    /// Part 1 runs these cores at their failing offsets and parks the other candidates.
    Part(CoreSet),
}

/// The start of a hunt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HuntStart {
    /// The hunt.
    pub hunt: u32,
    /// The cores it hunts among.
    pub candidates: CoreSet,
    /// Its first group, once one names it.
    pub first: Option<FirstGroup>,
}

impl HuntStart {
    fn sentence(&self) -> String {
        let hunt = self.hunt;
        match self.first {
            None => format!("#{hunt} started · candidates {}", core_ids(self.candidates)),
            Some(FirstGroup::Locate) => format!(
                "#{hunt} started · locate: {} at 0",
                core_ids(self.candidates)
            ),
            Some(FirstGroup::Part(cores)) => format!(
                "#{hunt} started · part 1: {}",
                part_layout(cores, self.candidates)
            ),
        }
    }
}

/// What an entry says, with the fields its sentence is built from.
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum What {
    /// A sentence kept as it was written, raw untrusted text in it.
    Said(String),
    /// A sentence about a hunt, kept as it was written.
    OfHunt {
        /// The hunt.
        hunt: u32,
        /// The sentence.
        text: String,
    },
    /// A sentence that lists items after its head, `head: a, b, c`; a narrow panel keeps whole items and counts
    /// the rest.
    List {
        /// What the items are of.
        head: String,
        /// The items.
        items: Vec<String>,
    },
    /// The session start.
    Session {
        /// The cores the session tunes.
        cores: u32,
        /// Solo limits carried into it from the last session.
        carried: u32,
    },
    /// A checking step's marker.
    Step(StepAt),
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
    /// Solo limits found at one offset.
    Limits {
        /// The cores.
        cores: CoreSet,
        /// The offset.
        offset: TargetOffset,
    },
    /// Neighbouring hunt groups answered alike by existing evidence.
    Answered {
        /// The groups.
        groups: Groups,
        /// The cores they load.
        cores: CoreSet,
        /// The answer.
        inferred: Inference,
    },
    /// A trial skipped because it already failed at these offsets.
    KnownFailure(KnownFailure),
    /// The start of a hunt.
    HuntStart(HuntStart),
}

/// One line of what happened, in plain words or merged from several events.
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
 #[derive(Debug, Clone, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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

    /// The hunt the line belongs to.
    pub fn hunt(&self) -> Option<u32> {
        match &self.what {
            What::OfHunt { hunt, .. } | What::HuntStart(HuntStart { hunt, .. }) => Some(*hunt),
            What::Answered { groups, .. } => Some(groups.hunt),
            What::Passes(passes) => match &passes.within {
                Some(PassedIn::Hunt { groups, .. }) => Some(groups.hunt),
                Some(PassedIn::Step(_)) | None => None,
            },
            What::Failed(failed) => failed.hunt,
            What::KnownFailure(known) => known.hunt.map(|located| located.hunt),
            What::Said(_)
            | What::List { .. }
            | What::Session { .. }
            | What::Step(_)
            | What::Reboot { .. }
            | What::Limits { .. } => None,
        }
    }

    /// The reboot the line reports.
    pub fn reboot(&self) -> Option<Seq> {
        match &self.what {
            What::Reboot { reboot, .. } => Some(*reboot),
            What::Failed(failed) => failed.reboot,
            What::Said(_)
            | What::OfHunt { .. }
            | What::List { .. }
            | What::Session { .. }
            | What::Step(_)
            | What::Passes(_)
            | What::Limits { .. }
            | What::Answered { .. }
            | What::KnownFailure(_)
            | What::HuntStart(_) => None,
        }
    }

    /// The checking step a marker names or passes ran in.
    pub fn step(&self) -> Option<&StepAt> {
        match &self.what {
            What::Step(at) => Some(at),
            What::Passes(passes) => passes.step(),
            What::Said(_)
            | What::OfHunt { .. }
            | What::List { .. }
            | What::Session { .. }
            | What::Failed(_)
            | What::Reboot { .. }
            | What::Limits { .. }
            | What::Answered { .. }
            | What::KnownFailure(_)
            | What::HuntStart(_) => None,
        }
    }

    /// The whole sentence as the plain-text consumers read it.
    pub fn text(&self) -> String {
        match &self.what {
            What::Said(text) | What::OfHunt { text, .. } => text.clone(),
            What::List { head, items } => format!("{head}: {}", items.join(", ")),
            What::Session { cores, carried } => {
                if *carried > 0 {
                    format!(
                        "session started · {} carried from the last session",
                        plural(*carried as usize, "solo limit")
                    )
                } else {
                    format!("session started on {cores} cores")
                }
            }
            What::Step(at) => at.label(),
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
            What::Limits { cores, offset } => {
                let offset = offset.get();
                let mut text = match (cores.first(), cores.len()) {
                    (Some(core), 1) => format!("core {:02} solo limit {offset}", core.get()),
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
                text
            }
            What::Answered {
                groups,
                cores,
                inferred,
            } => {
                let many = groups.last > groups.first;
                let range = if many {
                    format!("groups {}-{}", groups.first, groups.last)
                } else {
                    format!("group {}", groups.first)
                };
                let cores = core_ids(*cores);
                let (what, verdict) = if *inferred == Inference::Failure {
                    (cores, "failed on existing evidence")
                } else if many {
                    (
                        format!("parts of {cores}"),
                        "all passed on existing evidence",
                    )
                } else {
                    (cores, "passed on existing evidence")
                };
                format!("hunt {} {range} · {what} · {verdict}", groups.hunt)
            }
            What::KnownFailure(known) => known.sentence(),
            What::HuntStart(start) => start.sentence(),
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

    /// Merges `line` into this entry when it continues it: solo limits at the same offset, neighbouring groups
    /// answered alike, or passes of the same subject in the same step.
    fn absorb(&mut self, line: &Self) -> bool {
        match (&mut self.what, &line.what) {
            (
                What::Limits { cores, offset },
                What::Limits {
                    cores: more,
                    offset: at,
                },
            ) if offset == at => cores.extend(*more),
            (
                What::Answered {
                    groups,
                    cores,
                    inferred,
                },
                What::Answered {
                    groups: next,
                    cores: more,
                    inferred: answer,
                },
            ) if inferred == answer && groups.continued_by(next) => {
                groups.last = next.last;
                cores.extend(*more);
            }
            (What::Passes(kept), What::Passes(next))
                if kept.subject == next.subject && kept.step() == next.step() =>
            {
                kept.runs += next.runs;
                kept.of = kept.of.max(next.of);
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

/// Whether `line` is a member probe that passed every trial it needed, straight after one that did the same for
/// the hunt's previous group.
pub(crate) fn mergeable_probe_passes(last: &Entry, line: &Entry) -> bool {
    let (What::Passes(last), What::Passes(line)) = (&last.what, &line.what) else {
        return false;
    };
    let (Some(before), Some(after)) = (last.probe_groups(), line.probe_groups()) else {
        return false;
    };
    line.complete() && last.complete() && last.of == line.of && before.continued_by(after)
}

/// Folds adjacent member probes that each passed every trial they needed into one line.
pub(crate) fn merge_probe_passes(history: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::with_capacity(history.len());
    for entry in history {
        if let Some(last) = out.last_mut()
            && mergeable_probe_passes(last, &entry)
        {
            if let (What::Passes(kept), What::Passes(next)) = (&mut last.what, &entry.what) {
                if let (
                    Some(PassedIn::Hunt { groups, .. }),
                    Some(PassedIn::Hunt { groups: more, .. }),
                ) = (&mut kept.within, &next.within)
                {
                    groups.last = more.last;
                }
                kept.peak = kept.peak.max(next.peak);
            }
            last.at = entry.at;
            continue;
        }
        out.push(entry);
    }
    out
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

#[cfg(test)]
mod corpus_form {
    use super::{Entry, What};

    impl Entry {
        /// The name of the kind of sentence, as the narration corpus files it.
        pub(crate) const fn form(&self) -> &'static str {
            match self.what {
                What::Said(_) => "Said",
                What::OfHunt { .. } => "OfHunt",
                What::List { .. } => "List",
                What::Session { .. } => "Session",
                What::Step(_) => "Step",
                What::Passes(_) => "Passes",
                What::Failed(_) => "Failed",
                What::Reboot { .. } => "Reboot",
                What::Limits { .. } => "Limits",
                What::Answered { .. } => "Answered",
                What::KnownFailure(_) => "KnownFailure",
                What::HuntStart(_) => "HuntStart",
            }
        }
    }
}
