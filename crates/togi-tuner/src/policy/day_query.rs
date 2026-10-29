//! The day query: the intervals, events, vetoes, hold and step a day invocation owes.

use std::collections::HashSet;
use std::sync::Arc;

use crate::UnitProfile;
use jiff::Timestamp;
use togi_journal::{
    ApplicationId, DayHold, DayInterval, HoldChange, IntervalState, ObservedProfile, Seq,
};
use togi_machine::{BootId, ObservedOffset, PerCore};

use super::State;
use super::books::{Committed, Observed};
use super::decision::{Decision, PolicyPayload};
use super::night_query::NotReady;
use crate::day::{CoreProfile, IntervalKind, Invocation, Readback};

/// What a day invocation asks.
#[derive(Clone, Debug)]
pub struct DayQuery {
    /// The boot the invocation runs in.
    pub boot: BootId,
    pub invocation: Invocation,
    pub now: Timestamp,
}

const NS_PER_SEC: u64 = 1_000_000_000;

/// An interval the raw observer records of a boot imply.
struct Derived {
    boot: BootId,
    start_ns: u64,
    end_ns: u64,
    state: IntervalState,
    offsets: PerCore<ObservedOffset>,
    checkpoints: Vec<Seq>,
    reason: &'static str,
}

impl Derived {
    fn identity(&self) -> (BootId, u64, u64, IntervalState) {
        (self.boot.clone(), self.start_ns, self.end_ns, self.state)
    }
}

/// The committed intervals as the day policy reads them, typed from the first `folded` commitments, the
/// last of them at `last`.
#[derive(Clone, Debug, Default)]
pub(super) struct IntervalCache {
    folded: usize,
    last: Option<Seq>,
    intervals: Arc<Vec<crate::day::DayInterval>>,
}

/// One committed interval as the day policy reads it: it sits at its last checkpoint, the raw record
/// that proves it.
fn day_interval(commit: &Committed<DayInterval>) -> crate::day::DayInterval {
    let interval = &commit.payload;
    let kind = match (interval.state, &interval.profile) {
        (IntervalState::Applied, ObservedProfile::Ccd { ccd, .. }) => IntervalKind::Applied {
            profile: UnitProfile::of_ccd(ccd),
            readback: if interval.application.is_some() {
                Readback::Verified
            } else {
                Readback::Unverified
            },
        },
        (IntervalState::Applied | IntervalState::Ambiguous, _) => IntervalKind::Ambiguous,
        (IntervalState::Paused, _) => IntervalKind::Paused,
        (IntervalState::Suspended, _) => IntervalKind::Suspended,
        (IntervalState::Unobserved, _) => IntervalKind::Unobserved,
    };
    crate::day::DayInterval {
        at: interval.checkpoints.last().copied().unwrap_or(commit.seq),
        secs: interval.end_ns.saturating_sub(interval.start_ns) / NS_PER_SEC,
        kind,
    }
}

impl State {
    /// The committed intervals as the day policy reads them. An interval sits at its last checkpoint, the
    /// raw record that proves it, so a late commitment of old time never counts toward a later hold.
    /// Typed incrementally: commitments only append, so each is typed once.
    pub(super) fn committed_intervals(&self) -> Arc<Vec<crate::day::DayInterval>> {
        let commits = &self.days.intervals;
        let mut cache = self.caches.intervals.lock();
        let folded = cache.folded;
        let consistent = folded <= commits.len()
            && folded
                .checked_sub(1)
                .and_then(|at| commits.get(at))
                .map(|commit| commit.seq)
                == cache.last;
        if !consistent {
            *cache = IntervalCache::default();
        }
        if let Some(fresh) = commits
            .get(cache.folded..)
            .filter(|fresh| !fresh.is_empty())
        {
            Arc::make_mut(&mut cache.intervals).extend(fresh.iter().map(day_interval));
            cache.folded = commits.len();
            cache.last = commits.last().map(|commit| commit.seq);
        }
        Arc::clone(&cache.intervals)
    }

    /// The intervals the observer records of every boot imply.
    fn derive_intervals(&self) -> Vec<Derived> {
        let mut boots: Vec<_> = self.days.boots.iter().collect();
        boots.sort_by_key(|(_, records)| records.records.first().map(|record| record.seq));
        let mut derived = Vec::new();
        for (boot, records) in boots {
            let Some(first) = records.records.first() else {
                continue;
            };
            let monotonic = records
                .records
                .iter()
                .all(|record| record.mono_ns.is_some());
            let clock = |record: &super::books::Record| -> u64 {
                if monotonic {
                    record.mono_ns.unwrap_or(0)
                } else {
                    let wall = record.time.duration_since(first.time).as_nanos();
                    u64::try_from(wall).unwrap_or(0)
                }
            };
            let mut at_ns = clock(first);
            let mut offsets: Option<PerCore<ObservedOffset>> = None;
            let mut applied_before = 0_u64;
            let mut emit = |start_ns: u64,
                            end_ns: u64,
                            state: IntervalState,
                            offsets: PerCore<ObservedOffset>,
                            at: Seq,
                            reason: &'static str| {
                if end_ns > start_ns {
                    derived.push(Derived {
                        boot: boot.clone(),
                        start_ns,
                        end_ns,
                        state,
                        offsets,
                        checkpoints: vec![at],
                        reason,
                    });
                }
            };
            for record in &records.records {
                let now_ns = clock(record);
                match (&record.what, offsets) {
                    (Observed::Start { offsets: started }, _) => {
                        offsets = Some(*started);
                        at_ns = now_ns;
                        applied_before = 0;
                    }
                    (Observed::Checkpoint { applied_secs }, Some(held)) => {
                        let span = now_ns.saturating_sub(at_ns) / NS_PER_SEC;
                        let applied = applied_secs.saturating_sub(applied_before).min(span);
                        let split = at_ns.saturating_add(applied.saturating_mul(NS_PER_SEC));
                        emit(
                            at_ns,
                            split,
                            IntervalState::Applied,
                            held,
                            record.seq,
                            "applied seconds the checkpoint accounted for",
                        );
                        emit(
                            split,
                            now_ns,
                            IntervalState::Suspended,
                            held,
                            record.seq,
                            "time the checkpoint did not count as applied",
                        );
                        at_ns = now_ns;
                        applied_before = *applied_secs;
                    }
                    (Observed::Change { offsets: changed }, Some(held)) => {
                        emit(
                            at_ns,
                            now_ns,
                            IntervalState::Ambiguous,
                            held,
                            record.seq,
                            "the offsets changed inside it",
                        );
                        offsets = Some(*changed);
                        at_ns = now_ns;
                    }
                    (Observed::CleanStop, Some(held)) => {
                        emit(
                            at_ns,
                            now_ns,
                            IntervalState::Unobserved,
                            held,
                            record.seq,
                            "after the last checkpoint, before the clean stop",
                        );
                        at_ns = now_ns;
                    }
                    (
                        Observed::Checkpoint { .. } | Observed::Change { .. } | Observed::CleanStop,
                        None,
                    ) => {}
                }
            }
        }
        derived
    }

    /// The application whose profile the boot's observer read back as `offsets` before `at`.
    fn verified_application(
        &self,
        boot: &BootId,
        offsets: &PerCore<ObservedOffset>,
        at: Seq,
    ) -> Option<ApplicationId> {
        self.days
            .applications
            .iter()
            .rev()
            .find(|applied| {
                &applied.payload.boot == boot
                    && applied.seq < at
                    && applied
                        .payload
                        .target
                        .cores()
                        .iter()
                        .all(|(core, offset)| offsets.get(core) == Some(offset.observed()))
            })
            .map(|applied| applied.payload.application)
    }

    /// The `day.interval` records owed.
    pub(super) fn owed_day_intervals(&self) -> Vec<Decision> {
        let known: HashSet<(BootId, u64, u64, IntervalState)> = self
            .days
            .intervals
            .iter()
            .map(|commit| {
                let interval = &commit.payload;
                (
                    interval.boot.clone(),
                    interval.start_ns,
                    interval.end_ns,
                    interval.state,
                )
            })
            .collect();
        self.derive_intervals()
            .into_iter()
            .filter(|interval| !known.contains(&interval.identity()))
            .filter_map(|interval| {
                let profile = CoreProfile::new(interval.offsets).ok()?;
                let at = interval.checkpoints.last().copied()?;
                let application = match interval.state {
                    IntervalState::Applied => {
                        self.verified_application(&interval.boot, &interval.offsets, at)
                    }
                    IntervalState::Paused
                    | IntervalState::Suspended
                    | IntervalState::Unobserved
                    | IntervalState::Ambiguous => None,
                };
                Some(Decision::new(
                    PolicyPayload::DayInterval(DayInterval {
                        boot: interval.boot,
                        start_ns: interval.start_ns,
                        end_ns: interval.end_ns,
                        profile: self.observed_profile(&profile),
                        application,
                        state: interval.state,
                        checkpoints: interval.checkpoints.clone(),
                        reason: interval.reason.to_owned(),
                    }),
                    interval.checkpoints,
                ))
            })
            .collect()
    }

    /// The commitments of the intervals credited to the exact current D, with their seconds.
    pub(super) fn credited(&self) -> (Vec<Seq>, u64) {
        let Some(history) = self.days.history.as_ref() else {
            return (Vec::new(), 0);
        };
        let intervals = self.committed_intervals();
        let (seconds, witnesses) = crate::day::hold_accounting(
            &intervals,
            &history.current().profile,
            history.hold_origin(),
        );
        (
            witnesses
                .map(|index| self.days.intervals[index].seq)
                .collect(),
            seconds,
        )
    }

    /// The `day.hold` owed because the credit at the current D changed.
    fn owed_day_hold(&self) -> Result<Option<Decision>, NotReady> {
        let Some(history) = self.days.history.as_ref() else {
            return Ok(None);
        };
        let current = history.current();
        let Some(application) = self
            .days
            .applications
            .iter()
            .find(|applied| applied.seq == current.at)
        else {
            return Ok(None);
        };
        let (credited, eligible_s) = self.credited();
        let latest = self.days.holds.last();
        let same = latest.is_some_and(|hold| {
            hold.payload.application == application.payload.application
                && hold.payload.credited == credited
                && hold.payload.eligible_s == eligible_s
        });
        if same {
            return Ok(None);
        }
        let change = match latest {
            Some(hold) if hold.payload.application == application.payload.application => {
                match self.days.reclassified {
                    Some(cause) if cause > hold.seq => HoldChange::Recomputed { cause },
                    Some(_) | None => HoldChange::Credited {},
                }
            }
            Some(_) | None => HoldChange::Reset { cause: current.at },
        };
        Ok(Some(Decision::new(
            PolicyPayload::DayHold(DayHold {
                profile: self.ccd_vector(&current.profile)?,
                application: application.payload.application,
                credited: credited.clone(),
                eligible_s,
                change,
            }),
            credited,
        )))
    }


    /// What the day invocation owes, in the order the journal needs it: first what the night this boot
    /// ended owes at its close, one commitment at a time; then the observed intervals, events and vetoes,
    /// which are independent of one another; once those are journaled, the hold; and once that is, the one
    /// step, which cites the commitments before it. Append and fold what is returned, then ask again; an
    /// empty answer means the invocation owes nothing more.
    pub fn day(&self, query: &DayQuery) -> Result<Vec<Decision>, NotReady> {
        let closing = self
            .nights
            .plans
            .values()
            .next_back()
            .is_some_and(|book| book.closed_by.is_some());
        if closing {
            let known = self.known_contents(None);
            if let Some(owed) = self.with_layout(|layout| self.closure_owed(layout, &known))? {
                return Ok(vec![owed]);
            }
        }
        let mut owed = self.owed_day_intervals();
        owed.extend(self.owed_day_events(query.now)?);
        owed.extend(self.owed_day_vetoes()?);
        if !owed.is_empty() {
            return Ok(owed);
        }
        if let Some(hold) = self.owed_day_hold()? {
            return Ok(vec![hold]);
        }
        if let Some(commitment) = self.owed_climb_commitment(query)? {
            return Ok(vec![commitment]);
        }
        let view = self.day_view()?;
        Ok(self.owed_climb_decision(query, &view)?.into_iter().collect())
    }
}
