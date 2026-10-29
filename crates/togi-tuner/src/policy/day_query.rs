//! The day query: the intervals, events, vetoes, hold and step a day invocation owes.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use crate::UnitProfile;
use jiff::Timestamp;
use togi_journal::{
    ApplicationId, DayHold, DayInterval, DayStep, DecisionId, HoldChange, HoldReason,
    IntervalState, ObservedProfile, QualificationState, RetreatCause as JournalRetreatCause, Seq,
    StepCauses, StepKind,
};
use togi_machine::{BootId, ObservedOffset, PerCore};

use super::State;
use super::books::{Committed, Observed};
use super::day_events::DayView;
use super::decision::{Decision, PolicyPayload};
use super::night_query::NotReady;
use crate::day::{
    Blocker, CoreProfile, DayDecision, DayInputs, EventId, IntervalKind, Invocation, Readback,
    RetreatCause, day_step,
};

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
    fn credited(&self) -> (Vec<Seq>, u64) {
        let Some(history) = self.days.history.as_ref() else {
            return (Vec::new(), 0);
        };
        let applied = &history.current().profile;
        let origin = history.hold_origin();
        let mut seqs = Vec::new();
        let mut secs = 0_u64;
        for (commit, folded) in self
            .days
            .intervals
            .iter()
            .zip(self.committed_intervals().iter())
        {
            if let IntervalKind::Applied {
                profile,
                readback: Readback::Verified,
            } = &folded.kind
                && profile == applied
                && folded.at > origin
            {
                seqs.push(commit.seq);
                secs = secs.saturating_add(folded.secs);
            }
        }
        (seqs, secs)
    }

    /// The `day.hold` owed because the credit at the current D changed.
    fn owed_day_hold(&self) -> Result<Option<Decision>, NotReady> {
        let Some(history) = self.days.history.as_ref() else {
            return Ok(None);
        };
        let current = history.current();
        if current.profile.is_zero() {
            return Ok(None);
        }
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

    fn hold_reasons(
        &self,
        blockers: &[Blocker],
        proposal_profile: Option<&UnitProfile>,
    ) -> Result<Vec<HoldReason>, NotReady> {
        let mut reasons = Vec::new();
        for blocker in blockers {
            match blocker {
                Blocker::SameBoot => {}
                Blocker::PausedColdBoot => reasons.push(HoldReason::Paused {}),
                Blocker::Freeze => reasons.push(HoldReason::Frozen {}),
                Blocker::PendingQuestion { events } => {
                    reasons.extend(events.iter().filter_map(|event| match event {
                        EventId::Boot(boot) => {
                            Some(HoldReason::QuestionPending { boot: boot.clone() })
                        }
                        EventId::Unobserved(_) | EventId::Requalification(_) => None,
                    }));
                }
                Blocker::UnresolvedEvents { events } => {
                    reasons.extend(events.iter().filter_map(|event| {
                        self.journal_event_id(event)
                            .map(|event| HoldReason::EventUnresolved { event })
                    }));
                }
                Blocker::NothingToPromote => reasons.push(HoldReason::AtCap {}),
                Blocker::Vetoed { veto, .. } => {
                    reasons.extend(
                        self.journal_event_id(&veto.event)
                            .map(|veto| HoldReason::Vetoed { veto }),
                    );
                }
                Blocker::NotQualified { .. } => {
                    let expired = proposal_profile.is_some_and(|profile| {
                        self.qualification_records().iter().any(|record| {
                            profile.matches_ccd(record.start.payload.profile.ccd())
                                && self.is_current_record(record)
                                && matches!(
                                    record.latest.payload.state,
                                    QualificationState::Complete { .. }
                                )
                        })
                    });
                    reasons.push(if expired {
                        HoldReason::Expired {}
                    } else {
                        HoldReason::NotQualified {}
                    });
                }
                Blocker::QualificationFailed { proposal } => {
                    reasons.push(HoldReason::QualificationFailed {
                        profile: self.ccd_vector(&proposal.profile)?,
                        witnesses: self
                            .qualification_blocked(&proposal.profile)
                            .unwrap_or_default(),
                    });
                }
                Blocker::HoldIncomplete {
                    credited_secs,
                    required_secs,
                } => reasons.push(HoldReason::Incomplete {
                    credited_s: *credited_secs,
                    required_s: *required_secs,
                }),
            }
        }
        Ok(reasons)
    }

    /// The proposal a conclusive qualification failure durably blocks, if the next proposal is one:
    /// the day policy asks about that exact profile only, so a genuinely different proposal is free.
    fn blocked_proposals(
        &self,
        history: &crate::day::DayHistory,
        view: &DayView,
        caps: &UnitProfile,
    ) -> Result<BTreeSet<UnitProfile>, NotReady> {
        Ok(crate::day::next_proposal(history, &view.events, caps)?
            .map(|proposal| proposal.profile)
            .filter(|profile| self.qualification_blocked(profile).is_some())
            .into_iter()
            .collect())
    }

    fn event_commit(&self, id: &EventId) -> Option<Seq> {
        self.journal_event_id(id)
            .and_then(|id| self.days.events.get(&id))
            .map(|book| book.latest.seq)
    }

    fn qualification_seq(&self, profile: &UnitProfile) -> Option<Seq> {
        self.qualification_records()
            .iter()
            .filter(|record| {
                profile.matches_ccd(record.start.payload.profile.ccd())
                    && self.is_current_record(record)
                    && matches!(
                        record.latest.payload.state,
                        QualificationState::Complete { .. }
                    )
            })
            .map(|record| record.latest.seq)
            .max()
    }

    /// The `day.step` this invocation owes. A boot gets one hold or promotion, so once it has any
    /// step neither is owed again; a required retreat or firmware reset is owed whenever it differs
    /// from the profile the boot's latest step already proposed, so a same-boot restart that finds a
    /// firmware change or a cap under D still yields it, while an issued decision is never repeated.
    fn owed_day_step(
        &self,
        query: &DayQuery,
        view: &DayView,
    ) -> Result<Option<Decision>, NotReady> {
        let Some(history) = self.days.history.as_ref() else {
            return Ok(None);
        };
        let issued = self
            .days
            .steps
            .values()
            .rfind(|step| step.payload.cold_boot == query.boot);
        let intervals = self.committed_intervals();
        let reproductions = self.reproduction_outcomes();
        let caps = self.committed_caps()?;
        let qualified = self.qualified_profiles(query.now);
        let blocked = self.blocked_proposals(history, view, &caps)?;
        let decision = day_step(&DayInputs {
            topology: &self.machine.cores,
            history,
            events: &view.events,
            reproductions: &reproductions,
            intervals: intervals.as_slice(),
            caps: &caps,
            qualified: &qualified,
            blocked: &blocked,
            firmware: self.firmware_state(),
            freeze: self.days.freeze,
            invocation: query.invocation,
            params: self.params.hold,
        })?;
        if issued.is_some()
            && matches!(
                decision,
                DayDecision::Hold { .. } | DayDecision::Promote { .. }
            )
        {
            return Ok(None);
        }
        let applied = &history.current().profile;
        let old = self.ccd_vector(applied)?;
        let (credited, hold_s) = self.credited();
        let id = DecisionId(
            self.days
                .steps
                .keys()
                .next_back()
                .map_or(0, |last| last + 1),
        );
        let predecessor = self.days.steps.keys().next_back().copied().map(DecisionId);
        let changed = |to: &UnitProfile| -> Vec<togi_machine::CcdId> {
            to.iter()
                .filter(|&(ccd, offset)| applied.get(ccd) != Some(offset))
                .filter_map(|(unit, _)| unit.ccd())
                .collect()
        };
        let cap_commits: Vec<Seq> = self.nights.caps.values().map(|cap| cap.seq).collect();
        // Witnesses a step cites beyond its `causes`: a restoration's and a blocking failure's.
        let mut witnesses: Vec<Seq> = Vec::new();
        let (proposed, step, reason, causes, hours) = match decision {
            DayDecision::Hold { blockers } => {
                if matches!(query.invocation, Invocation::SameBoot) {
                    return Ok(None);
                }
                let proposal_profile =
                    crate::day::next_proposal(history, &view.events, &caps)?.map(|p| p.profile);
                let blocked_by = self.hold_reasons(&blockers, proposal_profile.as_ref())?;
                for reason in &blocked_by {
                    if let HoldReason::QualificationFailed {
                        witnesses: failures,
                        ..
                    } = reason
                    {
                        witnesses.extend(failures.iter().copied());
                    }
                }
                (
                    old.clone(),
                    StepKind::Hold { blocked_by },
                    "the day profile holds".to_owned(),
                    StepCauses {
                        caps: cap_commits,
                        ..StepCauses::default()
                    },
                    credited,
                )
            }
            DayDecision::Promote { proposal, .. } => {
                // A restoration cites the reclassified events and their last answers, the original
                // promotion and the step back it undoes (`docs/spec/tuner.md`, Undoing a
                // reclassified retreat).
                let (events, reason) = match &proposal.restores {
                    None => (
                        Vec::new(),
                        format!(
                            "promote CCD {} from {} to {}",
                            proposal.unit,
                            proposal.from.get(),
                            proposal.to.get()
                        ),
                    ),
                    Some(restoration) => {
                        witnesses.extend([restoration.promotion, restoration.retreat]);
                        witnesses.extend(restoration.events.iter().filter_map(|event| {
                            let EventId::Boot(boot) = event else {
                                return None;
                            };
                            self.days
                                .answers
                                .iter()
                                .rfind(|answer| &answer.boot == boot)
                                .map(|answer| answer.seq)
                        }));
                        (
                            restoration
                                .events
                                .iter()
                                .filter_map(|event| self.event_commit(event))
                                .collect(),
                            format!(
                                "restore CCD {} from {} to {}: the step back it undoes was reclassified",
                                proposal.unit,
                                proposal.from.get(),
                                proposal.to.get()
                            ),
                        )
                    }
                };
                (
                    self.ccd_vector(&proposal.profile)?,
                    StepKind::Promote {
                        changed: proposal.unit.ccd().ok_or(NotReady::NonCoarse)?,
                    },
                    reason,
                    StepCauses {
                        caps: cap_commits,
                        qualification: self.qualification_seq(&proposal.profile),
                        events,
                        ..StepCauses::default()
                    },
                    credited,
                )
            }
            DayDecision::Retreat {
                to,
                causes,
                unqualified,
            } => {
                let mut cited: Vec<EventId> = Vec::new();
                let mut cause = JournalRetreatCause::ShallowerCap {};
                let mut first = true;
                for item in &causes {
                    match item {
                        RetreatCause::StepBack { events, .. }
                        | RetreatCause::Escalation { events, .. } => {
                            cited.extend(events.iter().map(|event| event.id.clone()));
                            if first {
                                if let Some(event) = events.first() {
                                    let journal = self.journal_event_id(&event.id);
                                    let escalation = journal
                                        .as_ref()
                                        .and_then(|id| self.days.events.get(id))
                                        .and_then(|book| book.latest.payload.escalation);
                                    if let (Some(event), Some(escalation)) = (journal, escalation) {
                                        cause = JournalRetreatCause::Event { event, escalation };
                                        first = false;
                                    }
                                }
                            }
                        }
                        RetreatCause::Completion { events, .. } => {
                            cited.extend(events.iter().map(|event| event.id.clone()));
                            if first {
                                cause = JournalRetreatCause::Completion {};
                                first = false;
                            }
                        }
                        RetreatCause::SafetyCap { .. } => {}
                    }
                }
                let mut vetoes: Vec<Seq> = cited
                    .iter()
                    .filter_map(|id| self.journal_event_id(id))
                    .filter_map(|id| self.days.vetoes.get(&id).map(|veto| veto.seq))
                    .collect();
                vetoes.sort_unstable();
                let wanted = unqualified
                    .as_ref()
                    .map(|profile| self.ccd_vector(profile))
                    .transpose()?;
                (
                    self.ccd_vector(&to)?,
                    StepKind::Retreat {
                        changed: changed(&to),
                        cause,
                        wanted,
                    },
                    "step the day profile back".to_owned(),
                    StepCauses {
                        caps: cap_commits,
                        qualification: self.qualification_seq(&to),
                        vetoes,
                        events: cited
                            .iter()
                            .filter_map(|event| self.event_commit(event))
                            .collect(),
                    },
                    Vec::new(),
                )
            }
            DayDecision::FirmwareZero { to, .. } => (
                self.ccd_vector(&to)?,
                StepKind::FirmwareZero {
                    changed: applied
                        .iter()
                        .filter(|&(_, offset)| offset != togi_machine::TargetOffset::ZERO)
                        .filter_map(|(unit, _)| unit.ccd())
                        .collect(),
                },
                "the firmware context changed: reset to CO 0".to_owned(),
                StepCauses::default(),
                Vec::new(),
            ),
        };
        if issued.is_some_and(|step| step.payload.proposed == proposed) {
            return Ok(None);
        }
        let hold_s = match &step {
            StepKind::Hold { .. } | StepKind::Promote { .. } => hold_s,
            StepKind::Retreat { .. } | StepKind::FirmwareZero { .. } => 0,
        };
        let cites = {
            let mut cites = causes.caps.clone();
            cites.extend(causes.events.iter().copied());
            cites.extend(causes.vetoes.iter().copied());
            cites.extend(causes.qualification);
            cites.extend(witnesses);
            cites
        };
        Ok(Some(Decision::new(
            PolicyPayload::DayStep(DayStep {
                decision: id,
                cold_boot: query.boot.clone(),
                old,
                proposed,
                step,
                predecessor,
                hours,
                hold_s,
                causes,
                reason,
            }),
            cites,
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
        let view = self.day_view()?;
        Ok(self.owed_day_step(query, &view)?.into_iter().collect())
    }
}
