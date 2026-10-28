//! The history: one journal event turned into a line of what happened, or a change to the line an earlier event
//! began. The events whose line needs no earlier event are worded by the table in [`crate::words`]; the rest are
//! worded here, with the context they need.

use std::fmt::Write as _;

use jiff::Timestamp;
use togi_journal::{
    CrashDetected, Event, Outcome, Payload, State as JournalState, Tested, TrialEnd, TrialIntent,
};
use togi_machine::Signal;

use crate::facts::{Facts, trial_cores};
use crate::history::{Entry, Failed, Named, Passes, Verdict, What};
use crate::words::{Context, History, Tag, Tone, kind_words, lateness, on_cores, words};

fn entry(at: Timestamp, tag: Tag, text: String, tone: Tone) -> Entry {
    Entry::line(at, tag, text, tone)
}

impl Facts {
    /// Turns one event into a line of what happened, or changes an earlier line and returns nothing.
    pub(crate) fn describe(&mut self, event: &Event, _journal: &JournalState) -> Option<Entry> {
        let line = self.describe_event(event);
        #[cfg(test)]
        if let Some(line) = &line {
            crate::corpus::described(event, line);
        }
        line
    }

    fn describe_event(&mut self, event: &Event) -> Option<Entry> {
        let at = event.time;
        match words(event.kind()).history {
            History::Silent => None,
            History::Line(line) => {
                let line = line(&event.payload)?;
                Some(entry(at, line.tag, line.text, line.tone))
            }
            History::Recorded(tag, tone) => Some(entry(at, tag, event.msg.clone(), tone)),
            History::Contextual(context) => self.contextual(event, context),
        }
    }

    fn contextual(&mut self, event: &Event, context: Context) -> Option<Entry> {
        let at = event.time;
        match (context, &event.payload) {
            (Context::Start, Payload::SessionStart(start)) => Some(Entry::new(
                at,
                Tag::Start,
                What::Session {
                    cores: crate::numbers::saturating_ordinal(start.cores.len()),
                },
                Tone::Plain,
            )),
            (Context::Config, Payload::ConfigLoaded(_)) => (self.configs >= 2)
                .then(|| entry(at, Tag::Resume, "session resumed".to_owned(), Tone::Plain)),
            (Context::TrialEnd, Payload::TrialEnd(end)) => self.trial_ended(at, end, &event.cause),
            (Context::Crash, Payload::CrashDetected(crash)) => Some(self.crash(event, crash)),
            (Context::Start | Context::Config | Context::TrialEnd | Context::Crash, _) => None,
        }
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

    fn trial_ended(
        &mut self,
        at: Timestamp,
        d: &TrialEnd,
        cause: &[togi_journal::Seq],
    ) -> Option<Entry> {
        let intent = self.intents.get(&d.trial).cloned();
        let subject = intent
            .as_ref()
            .map_or_else(|| format!("trial {}", d.trial), trial_name);
        match d.outcome {
            Outcome::Pass => Some(Entry::new(
                at,
                Tag::Pass,
                What::Passes(Passes {
                    subject,
                    runs: 1,
                    peak: d.tctl_max_c,
                }),
                Tone::Good,
            )),
            Outcome::Failure => {
                let crashed = d.signal == Some(Signal::Crash);
                let (tag, when) = if crashed {
                    (Tag::Crash, lateness_text(d, intent.as_ref()))
                } else {
                    (Tag::Fail, None)
                };
                let mut failed = Failed {
                    subject,
                    verdict: Verdict {
                        signal: d.signal,
                        when,
                        named: d.core.map(Named::Core),
                    },
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
                Some(entry(
                    at,
                    Tag::Unclear,
                    format!("{subject} · {reason}"),
                    Tone::Warn,
                ))
            }
        }
    }
}

/// What a trial was for and what it loaded: "mapping CCD 1 at -15 · R2 heavy vector on cores 08-15". A Ruleset 13
/// intent names its activity, what it tested, its night task and whether it retries; an earlier one has only its
/// regime and cores.
fn trial_name(intent: &TrialIntent) -> String {
    let cores = trial_cores(intent);
    let mut load = format!(
        "{} {} {}",
        intent.regime,
        kind_words(intent.regime),
        on_cores(cores)
    );
    if intent.tested.is_none()
        && cores.len() == 1
        && let Some(offset) = cores.first().and_then(|core| intent.profile.get(core))
    {
        let _ = write!(load, " at {}", offset.get());
    }
    let mut parts = Vec::new();
    if let Some(activity) = intent.activity {
        parts.push(activity.as_str().to_owned());
    }
    match intent.tested {
        Some(Tested::Isolated { ccd, offset }) => {
            parts.push(format!("CCD {ccd} at {}", offset.get()));
        }
        Some(Tested::Exact {}) => parts.push("the exact profile".to_owned()),
        None => {}
    }
    if let Some(task) = intent.task {
        parts.push(format!("{} task {}", task.queue, task.ordinal));
    }
    if intent.retry {
        parts.push("retry".to_owned());
    }
    parts.push(load);
    parts.join(" · ")
}

fn lateness_text(d: &TrialEnd, intent: Option<&TrialIntent>) -> Option<String> {
    let intent = intent?;
    if intent.duration_s == 0 {
        return None;
    }
    let sample = d.last_sample_s.map(crate::facts::secs)?;
    let when = lateness(Some(sample), Some(crate::facts::secs(intent.duration_s)));
    Some(format!("{when} the trial"))
}
