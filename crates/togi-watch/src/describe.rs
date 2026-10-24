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
use togi_machine::{Condition, CoreSet, Regime, Signal};

use crate::facts::{Facts, HuntStartFacts, trial_cores};
use crate::history::{
    self, Entry, FirstGroup, Groups, KnownFailure, Located, Named, PassedIn, Passes, StepAt,
    Verdict, What,
};
use crate::words::{
    History, Tag, Tone, age, core_ids, cores_named, kind_words, lateness, on_cores, phase_line,
    plural, words,
};

fn seconds_between(from: Timestamp, to: Timestamp) -> std::time::Duration {
    crate::numbers::nonnegative_elapsed(
        i128::from(to.as_second() - from.as_second()) * 1_000_000_000,
    )
}

fn entry(at: Timestamp, tag: Tag, text: String, tone: Tone) -> Entry {
    Entry::line(at, tag, text, tone)
}

impl Facts {
    /// Turns one event into a line of what happened, or changes an earlier line and returns nothing.
    pub(crate) fn describe(&mut self, event: &Event, journal: &JournalState) -> Option<Entry> {
        let line = self.describe_event(event, journal);
        #[cfg(test)]
        if let Some(line) = &line {
            crate::corpus::described(event, line);
        }
        line
    }

    fn describe_event(&mut self, event: &Event, journal: &JournalState) -> Option<Entry> {
        let at = event.time;
        match words(event.kind()).history {
            History::Silent => None,
            History::Line(line) => {
                let mut line = line(&event.payload)?;
                self.finish_line(event, &mut line.text);
                Some(entry(at, line.tag, line.text, line.tone))
            }
            History::Contextual => self.contextual(event, journal),
        }
    }

    fn finish_line(&self, event: &Event, text: &mut String) {
        if let Payload::TunerDecision(decision) = &event.payload {
            if decision.decision == togi_journal::Decision::Backoff {
                if let Some(id) = self.sources.get(&event.seq) {
                    let _ = write!(text, " · clear of C{}", id.get());
                }
            }
        }
    }

    fn contextual(&mut self, event: &Event, journal: &JournalState) -> Option<Entry> {
        let at = event.time;
        match &event.payload {
            Payload::SessionStart(start) => Some(Entry::new(
                at,
                Tag::Start,
                What::Session {
                    cores: crate::numbers::saturating_ordinal(start.cores.len()),
                    carried: 0,
                },
                Tone::Plain,
            )),
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
            Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialCarried(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::TunerDecision(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
            | Payload::DayAnswer(_)
            | Payload::DayFault(_)
            | Payload::StateRebuilt(_) => None,
        }
    }

    fn failure(&self, at: Timestamp, d: &Failure) -> Option<Entry> {
        if let Some(known) = d.known_failure {
            let known = KnownFailure {
                regime: d.regime,
                crashed: d.signal == Signal::Crash,
                carried: self.carried.contains(&known),
                hunt: None,
            };
            return Some(Entry::new(
                at,
                Tag::Skip,
                What::KnownFailure(known),
                Tone::Plain,
            ));
        }
        crate::words::failure_line(d).map(|line| entry(at, line.tag, line.text, line.tone))
    }

    fn crash(&mut self, event: &Event, d: &CrashDetected) -> Entry {
        let line = if d.stray {
            entry(
                event.time,
                Tag::Crash,
                "before any offsets were applied, rebooted".to_owned(),
                Tone::Warn,
            )
        } else if d.inconclusive {
            entry(
                event.time,
                Tag::Reset,
                "power loss or reset, which says nothing about the offsets".to_owned(),
                Tone::Plain,
            )
        } else {
            Entry::new(
                event.time,
                Tag::Crash,
                What::Reboot {
                    idle: d.in_flight.is_none(),
                    reboot: event.seq,
                },
                Tone::Bad,
            )
        };
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
        if self.carrying
            && d.from == Some(CorePhase::Search)
            && d.to != CorePhase::Search
            && d.pass.is_some()
        {
            if let Some(What::Session { carried, .. }) = self
                .history
                .entries
                .iter_mut()
                .rev()
                .map(|entry| &mut entry.what)
                .find(|what| matches!(what, What::Session { .. }))
            {
                *carried += 1;
            }
            return None;
        }
        let line = phase_line(d)?;
        let what = if let (Tag::Limit, Some(offset)) = (line.tag, d.pass) {
            What::Limits {
                cores: std::iter::once(d.core).collect(),
                offset,
            }
        } else {
            What::Said(line.text)
        };
        Some(Entry::new(at, line.tag, what, line.tone))
    }

    /// The step at its place, with its regime when the cycle's plan is known.
    fn step_at(&self, cycle: u32, step: u32) -> StepAt {
        StepAt {
            cycle,
            step,
            regime: self
                .cycle_steps
                .get(&cycle)
                .and_then(|steps| steps.get(step as usize - 1))
                .copied(),
            workload: None,
        }
    }

    fn checking_step(&self, at: Timestamp, d: &CheckingStep) -> Entry {
        let at_step = self.step_at(d.cycle.get(), d.step.get());
        Entry::new(at, Tag::Step, What::Step(at_step), Tone::Plain)
    }

    fn step_workload(&mut self, intent: &TrialIntent) {
        let (Some(cycle), Some(step)) = (intent.cycle, intent.step) else {
            return;
        };
        let marker = self.history.entries.iter_mut().rev().find_map(|line| {
            if let What::Step(at) = &mut line.what
                && at.cycle == cycle.get()
                && at.step == step.get()
            {
                Some(at)
            } else {
                None
            }
        });
        if let Some(at) = marker {
            at.workload.get_or_insert(intent.workload);
        }
    }

    fn hunt_started(&mut self, at: Timestamp, d: &HuntStart) -> Option<Entry> {
        let hunt = d.hunt.get();
        self.hunt_starts.insert(
            hunt,
            HuntStartFacts {
                at,
                named: false,
                live: 0,
            },
        );
        if let Some(What::KnownFailure(known)) =
            self.history.entries.last_mut().map(|last| &mut last.what)
            && known.hunt.is_none()
        {
            known.hunt = Some(Located {
                hunt,
                cores: d.cores,
            });
            return None;
        }
        Some(Entry::new(
            at,
            Tag::Hunt,
            What::HuntStart(history::HuntStart {
                hunt,
                candidates: d.candidates,
                first: None,
            }),
            Tone::Warn,
        ))
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
                let opened = self.history.entries.iter_mut().rev().find_map(|line| {
                    if let What::HuntStart(opened) = &mut line.what
                        && opened.hunt == hunt
                    {
                        Some(opened)
                    } else {
                        None
                    }
                });
                if let Some(opened) = opened {
                    opened.first = Some(if d.stage == HuntStage::Locate {
                        FirstGroup::Locate
                    } else {
                        FirstGroup::Part(d.cores)
                    });
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
        if d.skipped {
            let text = format!(
                "hunt {hunt} group {group} skipped: {}",
                d.reason.as_deref().unwrap_or_default()
            );
            return Some(Entry::new(
                at,
                Tag::Skip,
                What::OfHunt { hunt, text },
                Tone::Plain,
            ));
        }
        let inferred = d.inferred?;
        if let Some(probe) = d.probe {
            let verb = if inferred == Inference::Pass {
                "passed"
            } else {
                "failed"
            };
            let text = format!(
                "hunt {hunt} group {group} · core {:02} at {} · {verb} on existing evidence",
                probe.core.get(),
                probe.offset.get()
            );
            return Some(Entry::new(
                at,
                Tag::Probe,
                What::OfHunt { hunt, text },
                Tone::Hunt,
            ));
        }
        Some(Entry::new(
            at,
            Tag::Group,
            What::Answered {
                groups: Groups::one(hunt, group),
                cores: d.cores,
                inferred,
            },
            Tone::Hunt,
        ))
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
                format!("{text} · {} named", cores_named(d.cores)),
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
                    let tuned = self
                        .tuned
                        .get(core)
                        .flatten()
                        .map_or(0, togi_machine::TargetOffset::get);
                    let verb = if offset.get() > tuned {
                        "yields"
                    } else {
                        "goes deeper"
                    };
                    Some(format!("core {:02} {verb} to {}", core.get(), offset.get()))
                })
                .collect();
            return Entry::new(
                at,
                Tag::Round,
                What::List {
                    head: format!("#{round}"),
                    items: moves,
                },
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
        let subject = match &intent {
            None => format!("trial {}", d.trial),
            Some(intent) => {
                if intent.condition == Condition::Alone && d.outcome == Outcome::Pass {
                    return None;
                }
                self.trial_name(intent, journal)
            }
        };
        let groups = intent
            .as_ref()
            .and_then(|i| i.hunt.zip(i.group))
            .map(|(hunt, group)| Groups::one(hunt.get(), group.get()));
        match d.outcome {
            Outcome::Pass => {
                let within = match groups {
                    Some(groups) => Some(PassedIn::Hunt {
                        groups,
                        probe: self
                            .groups
                            .get(&(groups.hunt, groups.first))
                            .is_some_and(|group| group.probe.is_some()),
                    }),
                    None => intent
                        .as_ref()
                        .filter(|intent| intent.hunt.is_none())
                        .and_then(|intent| {
                            Some(PassedIn::Step(StepAt {
                                workload: Some(intent.workload),
                                ..self.step_at(intent.cycle?.get(), intent.step?.get())
                            }))
                        }),
                };
                let passes = Passes {
                    subject,
                    runs: 1,
                    first: count.index,
                    of: count.of,
                    peak: d.tctl_max_c,
                    within,
                };
                Some(Entry::new(at, Tag::Pass, What::Passes(passes), Tone::Good))
            }
            Outcome::Failure => {
                let named = if let Some(core) = d.core {
                    Some(Named::Core(core))
                } else if intent
                    .as_ref()
                    .is_some_and(|i| i.hunt.is_none() && i.condition != Condition::Alone)
                {
                    Some(Named::Nobody)
                } else {
                    None
                };
                let crashed = d.signal == Some(Signal::Crash);
                let (tag, when) = if crashed {
                    (Tag::Crash, lateness_text(d, intent.as_ref(), count))
                } else {
                    (Tag::Fail, None)
                };
                let mut failed = history::Failed {
                    subject,
                    verdict: Verdict {
                        signal: d.signal,
                        when,
                        named,
                    },
                    hunt: groups.map(|groups| groups.hunt),
                    reboot: None,
                };
                if crashed
                    && let Some(i) =
                        self.history.entries.iter().rposition(|previous| {
                            previous.reboot().is_some_and(|r| cause.contains(&r))
                        })
                {
                    let previous = &self.history.entries[i];
                    failed.reboot = previous.reboot();
                    self.history.entries[i] =
                        Entry::new(previous.at, tag, What::Failed(failed), Tone::Bad);
                    return None;
                }
                Some(Entry::new(at, tag, What::Failed(failed), Tone::Bad))
            }
            Outcome::Inconclusive => {
                let reason = d.reason.as_deref().unwrap_or("it could not run");
                let text = format!("{subject} · {reason}");
                let what = match groups {
                    Some(groups) => What::OfHunt {
                        hunt: groups.hunt,
                        text,
                    },
                    None => What::Said(text),
                };
                Some(Entry::new(at, Tag::Unclear, what, Tone::Warn))
            }
        }
    }

    fn trial_name(&self, intent: &TrialIntent, journal: &JournalState) -> String {
        let cores = trial_cores(intent);
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
                let mut members = g.cores;
                members.extend(g.held.iter().map(|held| held.core));
                members.insert(probe.core);
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
        let mut text = format!("{what} {}", on_cores(cores));
        if cores.len() == 1
            && let Some(offset) = cores.first().and_then(|core| intent.profile.get(core))
        {
            let _ = write!(text, " at {}", offset.get());
        }
        match (intent.rerun, intent.condition) {
            (true, Condition::Parked) => format!("rerun of {text} with every core at 0"),
            (true, Condition::Alone | Condition::Together) => format!("rerun of {text}"),
            (false, Condition::Alone | Condition::Together | Condition::Parked) => text,
        }
    }
}

/// Names the part of an R7 step a trial loads, from the CCDs of its cores.
pub(crate) fn part_name(intent: &TrialIntent, journal: &JournalState) -> String {
    let loaded = trial_cores(intent);
    let mut ccds: Vec<String> = Vec::new();
    for core in &journal.cores {
        let ccd = core.ccd.get().to_string();
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
pub(crate) fn part_layout(cores: CoreSet, candidates: CoreSet) -> String {
    let mut text = format!("{} at failing offsets", core_ids(cores));
    let rest: CoreSet = candidates.iter().filter(|c| !cores.contains(*c)).collect();
    if !rest.is_empty() {
        let _ = write!(text, ", {} parked", core_ids(rest));
    }
    text
}
