//! Observed day totals beside the CO 0 totals, for `togi status` and the dashboard (`docs/spec/tuner.md`,
//! Evidence). Plain counts and hours from the raw day records, never a rate: neither total is a bound.
//!
//! Hours fold through the journal's [`DayIntervals`], and unclean ends and answers through its
//! [`DayQuestions`], so the totals agree with the per-profile hours and question lines status prints.
//! A crash is an unclean end, a manually recorded crash, or an imported one; an end the operator
//! answered `power-off` is not a crash unless a manual record also names it.

use std::fmt::Write as _;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use togi_journal::{
    CrashedBoot, DayAnswerKind, DayIntervalCredit, DayIntervals, DayQuestions, Event, EventKind,
    IntervalState, Payload, Provenance,
};
use togi_machine::{BootId, ObservedOffset, PerCore};

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
enum CrashId {
    Boot(BootId),
    At(Timestamp),
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
struct Crash {
    id: CrashId,
    stock: bool,
    imported: bool,
    /// A manual record names it, so no answer can say it was not a crash.
    recorded: bool,
}

/// Hours and crashes of one scope, with what part of each came from imported records.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Tally {
    pub secs: u64,
    pub imported_secs: u64,
    pub crashes: u32,
    pub imported_crashes: u32,
}

/// Seconds as hours with one decimal: `12.5h`.
fn hours(secs: u64) -> String {
    let tenths = secs / 360;
    format!("{}.{}h", tenths / 10, tenths % 10)
}

impl Tally {
    /// `12.5h observed, 2 crashes`, with the imported part in parentheses when there is one.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut out = format!("{} observed", hours(self.secs));
        if self.imported_secs > 0 {
            let _ = write!(out, " ({} imported)", hours(self.imported_secs));
        }
        let _ = write!(out, ", {}", crashes(self.crashes));
        if self.imported_crashes > 0 {
            let _ = write!(out, " ({} imported)", self.imported_crashes);
        }
        out
    }
}

fn crashes(count: u32) -> String {
    format!("{count} crash{}", if count == 1 { "" } else { "es" })
}

/// `1 unknown interval` / `2 unknown intervals`.
#[must_use]
pub fn unknown_intervals(count: u32) -> String {
    format!(
        "{count} unknown interval{}",
        if count == 1 { "" } else { "s" }
    )
}

/// The day records folded to totals: applied hours per profile class and the crashes.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct DayTotals {
    questions: DayQuestions,
    boots: DayIntervals,
    crashes: Vec<Crash>,
    all: [u64; 2],
    stock: [u64; 2],
    unknown: u32,
    imported: Vec<Provenance>,
}

fn is_stock(offsets: &PerCore<ObservedOffset>) -> bool {
    offsets.iter().next().is_some() && offsets.iter().all(|(_, offset)| offset.0 == 0)
}

impl DayTotals {
    pub fn of(events: &[Event]) -> Self {
        let mut totals = Self::default();
        for event in events {
            totals.apply(event);
        }
        totals
    }

    /// Folds one event, in journal order.
    pub fn apply(&mut self, event: &Event) {
        self.fold(event, false);
        if let Payload::DayImported(imported) = &event.payload {
            if self.imported.contains(&imported.original) {
                return;
            }
            self.imported.push(imported.original);
            if let Some(payload) = decode(imported.original_kind, &imported.raw) {
                let raw = Event {
                    seq: event.seq,
                    time: imported.time,
                    boot: imported.boot.clone(),
                    mono: None,
                    cause: Vec::new(),
                    msg: String::new(),
                    payload,
                };
                self.fold(&raw, true);
            }
        }
    }

    fn fold(&mut self, event: &Event, imported: bool) {
        self.questions.apply(event);
        if let Some(DayIntervalCredit::Credited {
            offsets, seconds, ..
        }) = self.boots.apply(event)
        {
            let stock = is_stock(offsets);
            self.credit(stock, imported, seconds);
        }
        if let Payload::DayUncleanEnd(end) = &event.payload {
            let id = CrashId::Boot(end.boot.clone());
            self.crash(id, is_stock(&end.offsets), imported, false);
        }
        if let Payload::DayRecord(record) = &event.payload {
            let id = match &record.boot {
                CrashedBoot::UncleanEnd { boot } => CrashId::Boot(boot.clone()),
                CrashedBoot::Unobserved { at } => CrashId::At(*at),
            };
            self.crash(id, is_stock(record.offsets.get()), imported, true);
        }
        if let Payload::DayInterval(interval) = &event.payload {
            if matches!(
                interval.state,
                IntervalState::Unobserved | IntervalState::Ambiguous
            ) {
                self.unknown = self.unknown.saturating_add(1);
            }
        }
    }

    fn credit(&mut self, stock: bool, imported: bool, secs: u64) {
        let slot = usize::from(imported);
        self.all[slot] = self.all[slot].saturating_add(secs);
        if stock {
            self.stock[slot] = self.stock[slot].saturating_add(secs);
        }
    }

    fn crash(&mut self, id: CrashId, stock: bool, imported: bool, recorded: bool) {
        if let Some(known) = self.crashes.iter_mut().find(|known| known.id == id) {
            known.recorded |= recorded;
            return;
        }
        self.crashes.push(Crash {
            id,
            stock,
            imported,
            recorded,
        });
    }

    fn counts(&self, crash: &Crash) -> bool {
        let CrashId::Boot(boot) = &crash.id else {
            return true;
        };
        crash.recorded
            || self
                .questions
                .answer(boot)
                .is_none_or(|answer| answer.answer != DayAnswerKind::PowerOff)
    }

    fn tally(&self, secs: [u64; 2], only_stock: bool) -> Tally {
        let mut tally = Tally {
            secs: secs[0].saturating_add(secs[1]),
            imported_secs: secs[1],
            ..Tally::default()
        };
        for crash in self
            .crashes
            .iter()
            .filter(|crash| (!only_stock || crash.stock) && self.counts(crash))
        {
            tally.crashes = tally.crashes.saturating_add(1);
            if crash.imported {
                tally.imported_crashes = tally.imported_crashes.saturating_add(1);
            }
        }
        tally
    }

    /// Every profile.
    #[must_use]
    pub fn all(&self) -> Tally {
        self.tally(self.all, false)
    }

    /// Profiles whose every read-back offset is 0.
    #[must_use]
    pub fn stock(&self) -> Tally {
        self.tally(self.stock, true)
    }

    /// Recorded intervals whose profile or time togi could not establish.
    #[must_use]
    pub const fn unknown_intervals(&self) -> u32 {
        self.unknown
    }

    /// No day boot, crash or unknown interval has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.boots.is_empty() && self.crashes.is_empty() && self.unknown == 0
    }
}

/// The payload of an imported raw day record, when it is a kind the totals fold and it decodes.
fn decode(kind: EventKind, raw: &str) -> Option<Payload> {
    if kind == EventKind::DayBootStart {
        return serde_json::from_str(raw).ok().map(Payload::DayBootStart);
    }
    if kind == EventKind::DayOffsetsChanged {
        return serde_json::from_str(raw)
            .ok()
            .map(Payload::DayOffsetsChanged);
    }
    if kind == EventKind::DayCheckpoint {
        return serde_json::from_str(raw).ok().map(Payload::DayCheckpoint);
    }
    if kind == EventKind::DayUncleanEnd {
        return serde_json::from_str(raw).ok().map(Payload::DayUncleanEnd);
    }
    if kind == EventKind::DayAnswer {
        return serde_json::from_str(raw).ok().map(Payload::DayAnswer);
    }
    if kind == EventKind::DayRecord {
        return serde_json::from_str(raw).ok().map(Payload::DayRecord);
    }
    None
}

#[cfg(test)]
mod tests;
