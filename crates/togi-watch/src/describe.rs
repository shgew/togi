//! The history: one journal event turned into a line of what happened, or a change to the line an earlier event
//! began. The events whose line needs no earlier event are worded by the table in [`crate::words`]; the rest are
//! worded here, with the context they need.

use jiff::Timestamp;
use std::fmt::Write as _;
use togi_journal::{
    CheckingStep, CorePhase, CoreTransition, CrashDetected, DeepeningRound, Event, Failure,
    HuntEnd, HuntGroup, HuntOutcome, HuntStage, HuntStart, Inference, Outcome, Payload,
    State as JournalState, TrialEnd, TrialIntent,
};
use togi_machine::{Condition, CoreId, Regime, Signal};

use crate::facts::{Facts, HuntStartFacts, trial_cores};
use crate::history::{Entry, EntryKind, Key, fold_entry};
use crate::words::{
    History, Tag, Tone, age, core_ids, kind_words, lateness, on_cores, phase_line, plural,
    signal_words, solo_limit, words, workload_display,
};

fn seconds_between(from: Timestamp, to: Timestamp) -> std::time::Duration {
    std::time::Duration::from_secs(u64::try_from(to.as_second() - from.as_second()).unwrap_or(0))
}

fn entry(at: Timestamp, tag: Tag, text: String, tone: Tone) -> Entry {
    Entry::line(at, tag, text, tone)
}

impl Facts {
    /// Turns one event into a line of what happened, or changes an earlier line and returns nothing.
    pub(crate) fn describe(&mut self, event: &Event, journal: &JournalState) -> Option<Entry> {
        let at = event.time;
        match words(event.kind()).history {
            History::Silent => None,
            History::Line(line) => {
                let line = line(&event.payload)?;
                let mut line = entry(at, line.tag, line.text, line.tone);
                self.finish_line(event, &mut line);
                Some(line)
            }
            History::Contextual => self.contextual(event, journal),
        }
    }

    fn finish_line(&self, event: &Event, line: &mut Entry) {
        if let Payload::TunerDecision(decision) = &event.payload {
            if decision.decision == togi_journal::Decision::Backoff {
                if let Some(id) = self.sources.get(&event.seq) {
                    let _ = write!(line.text, " · clear of C{}", id.get());
                }
            }
        }
    }

    fn contextual(&mut self, event: &Event, journal: &JournalState) -> Option<Entry> {
        let at = event.time;
        match &event.payload {
            Payload::SessionStart(start) => {
                let mut line = entry(at, Tag::Start, String::new(), Tone::Plain);
                line.kind = EntryKind::Session;
                line.of = u32::try_from(start.cores.len()).unwrap_or(u32::MAX);
                Some(line)
            }
            Payload::SessionCarried(_) => {
                self.carrying = true;
                None
            }
            Payload::ConfigLoaded(_) => (self.configs >= 2)
                .then(|| entry(at, Tag::Resume, "session resumed".to_owned(), Tone::Plain)),
            Payload::TrialIntent(intent) => {
                self.carrying = false;
                self.step_workload(intent);
                None
            }
            Payload::TrialEnd(end) => self.trial_ended(at, end, &event.cause, journal),
            Payload::Failure(failure) => self.failure(at, failure),
            Payload::CrashDetected(crash) => Some(self.crash(event, crash)),
            Payload::CorePhase(transition) => self.core_phase(at, transition),
            Payload::CheckingStep(step) => Some(self.checking_step(at, step)),
            Payload::HuntStart(start) => self.hunt_started(at, start),
            Payload::HuntGroup(group) => self.hunt_group(at, group),
            Payload::HuntEnd(end) => Some(self.hunt_end(at, end)),
            Payload::DeepeningRound(round) => Some(self.round(at, round)),
            _ => None,
        }
    }

    fn failure(&self, at: Timestamp, d: &Failure) -> Option<Entry> {
        if let Some(known) = d.known_failure {
            let verb = if d.signal == Signal::Crash {
                "crashed"
            } else {
                "failed"
            };
            let what = d.regime.map_or_else(
                || "a trial".to_owned(),
                |regime| format!("{regime} {}", kind_words(regime)),
            );
            let mut text = format!("{what} at these offsets already {verb}");
            if self.carried.contains(&known) {
                text.push_str(" in a carried trial");
            }
            let mut line = entry(at, Tag::Skip, text, Tone::Plain);
            line.key = Some(Key::KnownFailure);
            return Some(line);
        }
        crate::words::failure_line(d).map(|line| entry(at, line.tag, line.text, line.tone))
    }

    fn crash(&mut self, event: &Event, d: &CrashDetected) -> Entry {
        let (tag, text, tone) = if d.stray {
            (
                Tag::Crash,
                "before any offsets were applied, rebooted",
                Tone::Warn,
            )
        } else if d.inconclusive {
            (
                Tag::Reset,
                "power loss or reset, which says nothing about the offsets",
                Tone::Plain,
            )
        } else if d.in_flight.is_none() {
            (
                Tag::Crash,
                "idle with the offsets applied, rebooted",
                Tone::Bad,
            )
        } else {
            (Tag::Crash, "with the offsets applied, rebooted", Tone::Bad)
        };
        let mut line = entry(event.time, tag, text.to_owned(), tone);
        if !d.stray && !d.inconclusive {
            line.reboot = Some(event.seq);
        }
        if self
            .history
            .entries
            .last()
            .is_some_and(|last| last.tag == Tag::Resume)
        {
            self.history.entries.pop();
        }
        line
    }

    fn core_phase(&mut self, at: Timestamp, d: &CoreTransition) -> Option<Entry> {
        if self.carrying && d.from == Some(CorePhase::Search) && d.to != CorePhase::Search {
            if let Some(session) = self
                .history
                .entries
                .iter_mut()
                .rev()
                .find(|entry| entry.kind == EntryKind::Session)
            {
                session.count += 1;
            }
            return None;
        }
        let line = phase_line(d)?;
        let mut line = entry(at, line.tag, line.text, line.tone);
        if line.tag == Tag::Limit {
            line.kind = EntryKind::Limits;
            line.cores = vec![d.core];
            line.offset = Some(togi_machine::TargetOffset::clamped(solo_limit(d)));
            line.key = Some(Key::Limit);
        }
        Some(line)
    }

    fn checking_step(&self, at: Timestamp, d: &CheckingStep) -> Entry {
        let (cycle, step) = (d.cycle.get(), d.step.get());
        let mut text = format!("cycle {cycle} step {step}");
        if let Some(regime) = self
            .cycle_steps
            .get(&cycle)
            .and_then(|steps| steps.get(step as usize - 1))
        {
            let _ = write!(text, ": {regime} {}", kind_words(*regime));
        }
        let mut line = entry(at, Tag::Step, text, Tone::Plain);
        line.key = Some(Key::Step { cycle, step });
        line
    }

    fn step_workload(&mut self, intent: &TrialIntent) {
        let (Some(cycle), Some(step)) = (intent.cycle, intent.step) else {
            return;
        };
        let key = Key::Step {
            cycle: cycle.get(),
            step: step.get(),
        };
        if let Some(line) = self
            .history
            .entries
            .iter_mut()
            .rev()
            .find(|line| line.key.as_ref() == Some(&key))
        {
            if !line.text.contains(", ") {
                let _ = write!(line.text, ", {}", workload_display(intent.workload));
            }
        }
    }

    fn hunt_started(&mut self, at: Timestamp, d: &HuntStart) -> Option<Entry> {
        let hunt = d.hunt.get();
        self.hunt_starts.insert(
            hunt,
            HuntStartFacts {
                at,
                candidates: d.candidates,
                named: false,
                live: 0,
            },
        );
        if let Some(last) = self.history.entries.last_mut() {
            if last.key == Some(Key::KnownFailure) && last.hunt.is_none() {
                last.hunt = Some(hunt);
                last.text = last.text.replacen(
                    " at these",
                    &format!(" on {} at these", core_ids(d.cores)),
                    1,
                );
                let _ = write!(last.text, " → hunt {hunt}");
                return None;
            }
        }
        let mut line = entry(
            at,
            Tag::Hunt,
            format!("#{hunt} started · candidates {}", core_ids(d.candidates)),
            Tone::Warn,
        );
        line.hunt = Some(hunt);
        line.key = Some(Key::HuntStart);
        Some(line)
    }

    fn hunt_group(&mut self, at: Timestamp, d: &HuntGroup) -> Option<Entry> {
        let (hunt, group) = (d.hunt.get(), d.group.get());
        self.groups.insert((hunt, group), d.clone());
        let mut start = self.hunt_starts.get(&hunt).cloned();
        if let Some(start) = start.as_mut() {
            if d.probe.is_none()
                && !start.named
                && (!d.cores.is_empty() || d.stage == HuntStage::Locate)
            {
                start.named = true;
                if let Some(line) = self
                    .history
                    .entries
                    .iter_mut()
                    .rev()
                    .find(|line| line.key == Some(Key::HuntStart) && line.hunt == Some(hunt))
                {
                    line.text = if d.stage == HuntStage::Locate {
                        format!(
                            "#{hunt} started · locate: {} at 0",
                            core_ids(start.candidates)
                        )
                    } else {
                        format!(
                            "#{hunt} started · part 1: {}",
                            part_layout(d.cores, start.candidates)
                        )
                    };
                }
            }
            if d.inferred.is_none() && !d.skipped {
                start.live += 1;
            }
            self.hunt_starts.insert(hunt, start.clone());
        }
        if d.inferred.is_none() && !d.skipped {
            return None;
        }
        let mut line = entry(at, Tag::Skip, String::new(), Tone::Plain);
        line.hunt = Some(hunt);
        line.first_group = group;
        line.last_group = group;
        if d.skipped {
            line.text = format!(
                "hunt {hunt} group {group} skipped: {}",
                d.reason.as_deref().unwrap_or_default()
            );
        } else if let Some(probe) = d.probe {
            let verb = if d.inferred == Some(Inference::Pass) {
                "passed"
            } else {
                "failed"
            };
            line.tag = Tag::Probe;
            line.tone = Tone::Hunt;
            line.text = format!(
                "hunt {hunt} group {group} · core {:02} at {} · {verb} on existing evidence",
                probe.core.get(),
                probe.offset.get()
            );
        } else if let Some(inferred) = d.inferred {
            line.kind = EntryKind::Groups;
            line.tag = Tag::Group;
            line.tone = Tone::Hunt;
            line.cores = d.cores.iter().collect();
            line.inferred = Some(inferred);
            line.key = Some(Key::Carried { hunt, inferred });
        }
        Some(line)
    }

    fn hunt_end(&self, at: Timestamp, d: &HuntEnd) -> Entry {
        let hunt = d.hunt.get();
        let start = self.hunt_starts.get(&hunt);
        let mut text = format!("#{hunt} done");
        if d.groups > 0 {
            let _ = write!(text, " · {}", plural(d.groups as usize, "group"));
        }
        if start.is_some_and(|start| start.live == 0) && d.groups > 0 {
            text.push_str(", all answered by existing evidence");
        } else if let (Some(start), true) = (start, d.groups > 0) {
            let _ = write!(text, " in {}", age(seconds_between(start.at, at)));
        }
        let cores = core_ids(d.cores);
        let (text, tone) = match d.result {
            HuntOutcome::Culprit | HuntOutcome::Direct => (
                format!(
                    "{text} · {} named",
                    only_core(&d.cores.iter().collect::<Vec<_>>())
                ),
                Tone::Good,
            ),
            HuntOutcome::Combination => {
                let probed = if self.probes.contains(&hunt) {
                    ", probed"
                } else {
                    ""
                };
                (
                    format!("{text} · {cores} kept together{probed}"),
                    Tone::Warn,
                )
            }
            HuntOutcome::Fallback => (format!("{text} · {cores} unresolved"), Tone::Warn),
            HuntOutcome::Loaded => (format!("{text} · stays with loaded {cores}"), Tone::Warn),
            HuntOutcome::Cancelled => (format!("#{hunt} cancelled"), Tone::Plain),
        };
        entry(at, Tag::Hunt, text, tone)
    }

    fn round(&self, at: Timestamp, d: &DeepeningRound) -> Entry {
        let round = d.round.get();
        if d.event == togi_journal::RoundEvent::Start {
            let moves: Vec<String> = d
                .cores
                .iter()
                .filter_map(|core| {
                    let offset = d.profile.get(core)?;
                    let tuned = self.tuned.get(&core).map_or(0, |tuned| tuned.get());
                    let verb = if offset.get() > tuned {
                        "yields"
                    } else {
                        "goes deeper"
                    };
                    Some(format!("core {:02} {verb} to {}", core.get(), offset.get()))
                })
                .collect();
            return entry(
                at,
                Tag::Round,
                format!("#{round}: {}", moves.join(", ")),
                Tone::Plain,
            );
        }
        if d.passed {
            return entry(
                at,
                Tag::Round,
                format!("#{round} passed, the proposed offsets held"),
                Tone::Good,
            );
        }
        entry(
            at,
            Tag::Round,
            format!(
                "#{round} stopped: {}",
                d.reason.as_deref().unwrap_or_default()
            ),
            Tone::Warn,
        )
    }

    fn trial_ended(
        &mut self,
        at: Timestamp,
        d: &TrialEnd,
        cause: &[togi_journal::Seq],
        journal: &JournalState,
    ) -> Option<Entry> {
        let intent = self.intents.get(&d.trial).cloned();
        let count = self.counts.get(&d.trial).copied().unwrap_or_default();
        let mut line = entry(at, Tag::Unclear, String::new(), Tone::Plain);
        match &intent {
            None => line.text = format!("trial {}", d.trial),
            Some(intent) => {
                if intent.condition == Condition::Alone && d.outcome == Outcome::Pass {
                    return None;
                }
                line.text = self.trial_name(intent, journal);
            }
        }
        line.first = count.index;
        line.of = count.of;
        line.runs = 1;
        if let Some((hunt, group)) = intent.as_ref().and_then(|i| i.hunt.zip(i.group)) {
            line.hunt = Some(hunt.get());
            line.first_group = group.get();
            line.last_group = group.get();
            line.probe = self
                .groups
                .get(&(hunt.get(), group.get()))
                .is_some_and(|group| group.probe.is_some());
        }
        match d.outcome {
            Outcome::Pass => {
                line.kind = EntryKind::Trials;
                line.tag = Tag::Pass;
                line.tone = Tone::Good;
                line.peak = d.tctl_max_c;
                line.key = Some(Key::Pass(line.text.clone()));
            }
            Outcome::Failure => {
                let named = if let Some(core) = d.core {
                    format!(" · core {:02} named", core.get())
                } else if intent
                    .as_ref()
                    .is_some_and(|i| i.hunt.is_none() && i.condition != Condition::Alone)
                {
                    " · no core named".to_owned()
                } else {
                    String::new()
                };
                if d.signal == Some(Signal::Crash) {
                    line.tag = Tag::Crash;
                    line.tone = Tone::Bad;
                    line.text.push_str(" · rebooted");
                    if let Some(late) = lateness_text(d, intent.as_ref(), count) {
                        let _ = write!(line.text, " · {late}");
                    }
                } else {
                    let words = d.signal.map_or("failed", signal_words);
                    line.tag = Tag::Fail;
                    line.tone = Tone::Bad;
                    let _ = write!(line.text, " · {words}");
                    line.alarm = Some(words.to_owned());
                }
                line.text.push_str(&named);
                if d.signal == Some(Signal::Crash) {
                    if let Some(i) =
                        self.history.entries.iter().rposition(|previous| {
                            previous.reboot.is_some_and(|r| cause.contains(&r))
                        })
                    {
                        let previous = &self.history.entries[i];
                        line.reboot = previous.reboot;
                        line.at = previous.at;
                        self.history.entries[i] = line.clone();
                        let last = self.history.entries.len() - 1;
                        if i == last && i > 0 {
                            let mut merged = self.history.entries[..i].to_vec();
                            fold_entry(&mut merged, line);
                            if merged.len() == i {
                                self.history.entries = merged;
                            }
                        }
                        return None;
                    }
                }
            }
            Outcome::Inconclusive => {
                let reason = d.reason.as_deref().unwrap_or("it could not run");
                line.tag = Tag::Unclear;
                line.tone = Tone::Warn;
                line.text = format!("{} · {reason}", line.text);
            }
        }
        Some(line)
    }

    fn trial_name(&self, intent: &TrialIntent, journal: &JournalState) -> String {
        let cores: Vec<CoreId> = trial_cores(intent).iter().collect();
        let regime = intent.regime;
        let mut what = format!("{regime} {}", kind_words(regime));
        if intent.condition == Condition::Alone {
            if let (Some(offset), Some(core)) = (intent.offset, cores.first()) {
                return format!("core {:02} · {what} at {}", core.get(), offset.get());
            }
        }
        if let (Some(hunt), Some(group)) = (intent.hunt, intent.group) {
            let text = format!("hunt {} group {}", hunt.get(), group.get());
            let Some(g) = self.groups.get(&(hunt.get(), group.get())) else {
                return text;
            };
            if let Some(probe) = g.probe {
                let mut members: Vec<CoreId> = g.cores.iter().collect();
                for held in &g.held {
                    if !members.contains(&held.core) {
                        members.push(held.core);
                    }
                }
                if !members.contains(&probe.core) {
                    members.push(probe.core);
                }
                return format!(
                    "{text} · {} with core {:02} at {}",
                    core_ids(members),
                    probe.core.get(),
                    probe.offset.get()
                );
            }
            if g.stage == HuntStage::Locate {
                return format!("{text} · locate, idle cores at 0");
            }
            return format!("{text} · {} at failing offsets", core_ids(g.cores));
        }
        if regime == Regime::R7 && intent.cycle.is_some() {
            what = format!("{regime} {}", part_name(intent, journal));
        }
        let mut text = format!("{what} {}", on_cores(&cores));
        if let [core] = cores.as_slice() {
            if let Some(offset) = intent.profile.get(*core) {
                let _ = write!(text, " at {}", offset.get());
            }
        }
        match (intent.rerun, intent.condition) {
            (true, Condition::Parked) => format!("rerun of {text} with every core at 0"),
            (true, _) => format!("rerun of {text}"),
            _ => text,
        }
    }
}

/// Names the part of an R7 step a trial loads, from the CCDs of its cores.
pub(crate) fn part_name(intent: &TrialIntent, journal: &JournalState) -> String {
    let loaded = trial_cores(intent);
    let mut ccds: Vec<String> = Vec::new();
    for core in &journal.cores {
        let ccd = core.ccd.0.to_string();
        if loaded.contains(core.core) && !ccds.contains(&ccd) {
            ccds.push(ccd);
        }
    }
    let partial = intent.regime == Regime::R7
        && journal.cores.iter().any(|core| {
            loaded.contains(core.core)
                && journal
                    .cores
                    .iter()
                    .any(|other| other.ccd == core.ccd && !loaded.contains(other.core))
        });
    if partial {
        format!("partial CCD {}", ccds.join("+"))
    } else {
        format!("full CCD {}", ccds.join("+"))
    }
}

fn lateness_text(
    d: &TrialEnd,
    intent: Option<&TrialIntent>,
    count: crate::model::Count,
) -> Option<String> {
    let intent = intent?;
    if intent.duration_s == 0 {
        return None;
    }
    let sample = d.last_sample_s.map(crate::facts::secs)?;
    let when = lateness(Some(sample), Some(crate::facts::secs(intent.duration_s)));
    Some(if count.of > 0 {
        format!("{when} trial {} of {}", count.index, count.of)
    } else {
        format!("{when} the trial")
    })
}

/// Says which candidates a hunt group runs at their failing offsets and which it parks.
pub(crate) fn part_layout(
    cores: togi_machine::CoreSet,
    candidates: togi_machine::CoreSet,
) -> String {
    let mut text = format!("{} at failing offsets", core_ids(cores));
    let rest: Vec<CoreId> = candidates.iter().filter(|c| !cores.contains(*c)).collect();
    if !rest.is_empty() {
        let _ = write!(text, ", {} parked", core_ids(rest));
    }
    text
}

fn only_core(cores: &[CoreId]) -> String {
    match cores {
        [core] => format!("core {:02}", core.get()),
        _ => format!("cores {}", core_ids(cores.iter().copied())),
    }
}
