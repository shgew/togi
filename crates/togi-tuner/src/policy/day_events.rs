//! Daily events and vetoes: the raw observer records classified by the day policy, and the `day.event`
//! and `day.veto` commitments that carry them.

use std::collections::{BTreeMap, HashMap};

use crate::UnitId;
use jiff::Timestamp;
use togi_journal::{
    CcdCursor, Certainty, Classification as JournalClassification, CrashedBoot, DayEvent, DayVeto,
    DecisionId, Escalation, LiveFault, ObservedProfile, QualificationPurpose, QualificationState,
    ReproductionState, Resolution, Seq, VetoState,
};
use togi_machine::{BootId, CcdProfile, ObservedOffset, PerCore, TargetOffset};

use super::State;
use super::decision::{Decision, PolicyPayload};
use super::night_query::NotReady;
use crate::day::{
    Answer, ApplicationKind, Certainty as DayCertainty, Classification, CoreProfile, DailyEvent,
    EventId, EventStatus, Invocation, MceKind, Observation, ReproductionOutcome, Witness,
    daily_events, event_statuses, vetoes,
};

/// Where an event's raw evidence came from.
#[derive(Clone, Debug)]
pub(super) struct EventMeta {
    pub(super) boot: Option<BootId>,
    pub(super) from: Timestamp,
    pub(super) to: Timestamp,
    pub(super) witnesses: Vec<Seq>,
}

/// The daily events as the day policy classifies them now.
#[derive(Clone, Debug)]
pub(super) struct DayView {
    pub(super) events: Vec<DailyEvent>,
    pub(super) meta: HashMap<EventId, EventMeta>,
}

/// The CPU evidence of a live machine check (`docs/spec/tuner.md`, Live faults and load mix): an
/// uncorrected one is CPU-suspect whatever its bank, a corrected one only when its decoded bank is
/// core-local (`docs/spec/workloads.md`, MCE bank classification). A shared or unknown bank (L3,
/// memory controller, data fabric) is not core-local even when a reporting core is known, and a
/// core-local bank is CPU evidence even when the topology did not resolve its core.
fn live_mce_kind(fault: &LiveFault) -> Option<MceKind> {
    match fault {
        LiveFault::MachineCheck {
            corrected: false, ..
        } => Some(MceKind::Uncorrected),
        LiveFault::MachineCheck {
            corrected: true,
            bank_type,
            ..
        } if bank_type.core_local() => Some(MceKind::CoreLocal),
        LiveFault::MachineCheck {
            corrected: true, ..
        }
        | LiveFault::KernelError { .. }
        | LiveFault::Trap { .. } => None,
    }
}

struct Source {
    id: EventId,
    seq: Seq,
    time: Timestamp,
}

/// The ledger indexes of the failing filler re-runs of exact cells among the first `scanned` records.
#[derive(Clone, Debug, Default)]
pub(super) struct FillerFailures {
    scanned: usize,
    failures: Vec<usize>,
}

impl State {
    /// The ledger indexes of every failing filler re-run of exact cells, in ledger order. The ledger
    /// only appends, so each record is examined once.
    fn filler_failures(&self) -> Vec<usize> {
        let records = &self.ledger.records;
        let mut cache = self.caches.filler_failures.lock();
        if cache.scanned > records.len() {
            *cache = FillerFailures::default();
        }
        let scanned = cache.scanned;
        for (index, exposure) in records.iter().enumerate().skip(scanned) {
            let filler_exact = exposure
                .task
                .is_some_and(|task| task.queue == togi_journal::Queue::Filler)
                && exposure.observed.tested == togi_journal::Tested::Exact {};
            if filler_exact && exposure.observed.outcome == togi_journal::Outcome::Failure {
                cache.failures.push(index);
            }
        }
        cache.scanned = records.len();
        cache.failures.clone()
    }

    /// The ledger record at `seq`: the ledger folds in journal order, so its seqs ascend.
    fn exposure_at(&self, seq: Seq) -> Option<&super::books::Exposure> {
        let records = &self.ledger.records;
        records
            .binary_search_by_key(&seq, |exposure| exposure.seq)
            .ok()
            .and_then(|at| records.get(at))
    }

    /// The machine-check evidence a boot's logs carry: an uncorrected one outranks a core-local one.
    fn mce_kind(&self, boot: &BootId) -> Option<MceKind> {
        let mut kind = None;
        for record in self.days.mces.iter().filter(|record| &record.boot == boot) {
            if !record.mce.corrected {
                return Some(MceKind::Uncorrected);
            }
            if record.mce.bank_type.core_local() {
                kind = Some(MceKind::CoreLocal);
            }
        }
        for record in self
            .days
            .live_mces
            .iter()
            .filter(|record| &record.boot == boot)
        {
            match live_mce_kind(&record.fault.fault) {
                Some(MceKind::Uncorrected) => return Some(MceKind::Uncorrected),
                Some(MceKind::CoreLocal) => kind = Some(MceKind::CoreLocal),
                None => {}
            }
        }
        kind
    }

    /// The offsets a boot last recorded.
    fn boot_offsets(&self, boot: &BootId) -> Option<PerCore<ObservedOffset>> {
        use super::books::Observed;
        self.days
            .boots
            .get(boot)?
            .records
            .iter()
            .rev()
            .find_map(|record| match &record.what {
                Observed::Start { offsets } | Observed::Change { offsets } => Some(*offsets),
                Observed::Checkpoint { .. } | Observed::CleanStop => None,
            })
    }

    /// The raw observations, each daily event's first and last evidence, and the classified events.
    pub(super) fn day_view(&self) -> Result<DayView, NotReady> {
        let mut observations: Vec<Observation> = Vec::new();
        let mut sources: Vec<Source> = Vec::new();
        let mut seen: Vec<&BootId> = Vec::new();
        for unclean in &self.days.unclean {
            if seen.contains(&&unclean.end.boot) {
                continue;
            }
            seen.push(&unclean.end.boot);
            let Ok(profile) = CoreProfile::new(unclean.end.offsets) else {
                continue;
            };
            let id = EventId::Boot(unclean.end.boot.clone());
            observations.push(Observation {
                id: id.clone(),
                profile,
                witness: Witness::UncleanEnd {
                    class: unclean.end.class(),
                    mce: self.mce_kind(&unclean.end.boot),
                },
            });
            sources.push(Source {
                id,
                seq: unclean.seq,
                time: unclean.time,
            });
        }
        let mut clean: Vec<&BootId> = Vec::new();
        for record in &self.days.mces {
            if seen.contains(&&record.boot) || clean.contains(&&record.boot) {
                continue;
            }
            clean.push(&record.boot);
            let (Some(kind), Some(offsets)) =
                (self.mce_kind(&record.boot), self.boot_offsets(&record.boot))
            else {
                continue;
            };
            let Ok(profile) = CoreProfile::new(offsets) else {
                continue;
            };
            let id = EventId::Boot(record.boot.clone());
            observations.push(Observation {
                id: id.clone(),
                profile,
                witness: Witness::CleanBootMce(kind),
            });
            sources.push(Source {
                id,
                seq: record.seq,
                time: record.time,
            });
        }
        for record in &self.days.live_mces {
            if seen.contains(&&record.boot)
                || clean.contains(&&record.boot)
                || live_mce_kind(&record.fault.fault).is_none()
            {
                continue;
            }
            clean.push(&record.boot);
            let Some(kind) = self.mce_kind(&record.boot) else {
                continue;
            };
            let Ok(profile) = CoreProfile::new(record.fault.offsets) else {
                continue;
            };
            let id = EventId::Boot(record.boot.clone());
            observations.push(Observation {
                id: id.clone(),
                profile,
                witness: Witness::CleanBootMce(kind),
            });
            sources.push(Source {
                id,
                seq: record.seq,
                time: record.time,
            });
        }
        for manual in &self.days.manual {
            let Ok(profile) = CoreProfile::new(*manual.record.offsets.get()) else {
                continue;
            };
            let id = match &manual.record.boot {
                CrashedBoot::UncleanEnd { boot } => EventId::Boot(boot.clone()),
                CrashedBoot::Unobserved { at } => EventId::Unobserved(*at),
            };
            observations.push(Observation {
                id: id.clone(),
                profile,
                witness: Witness::Manual,
            });
            sources.push(Source {
                id,
                seq: manual.seq,
                time: manual.time,
            });
        }
        for record in self.qualification_records().iter() {
            let failed = matches!(
                record.latest.payload.state,
                QualificationState::Failed { .. }
            ) && record.start.payload.purpose == QualificationPurpose::Requalification
                && self.is_current_record(record);
            let QualificationState::Failed { failure, .. } = &record.latest.payload.state else {
                continue;
            };
            if !failed {
                continue;
            }
            let Some(night) = self
                .exposure_at(*failure)
                .and_then(|exposure| exposure.night)
            else {
                continue;
            };
            let profile =
                CoreProfile::of_ccd(record.start.payload.profile.ccd(), &self.machine.cores)?;
            let id = EventId::Requalification(night.0);
            observations.push(Observation {
                id: id.clone(),
                profile,
                witness: Witness::FailedRequalification,
            });
            sources.push(Source {
                id,
                seq: record.latest.seq,
                time: record.latest.time,
            });
        }
        // A filler re-run of D's exact cells that fails takes the failed re-qualification path.
        for index in self.filler_failures() {
            let Some(exposure) = self.ledger.records.get(index) else {
                continue;
            };
            if !self.is_current(&exposure.observed) {
                continue;
            }
            let (Some(night), Some(ended)) = (
                exposure.night,
                exposure
                    .observed
                    .source
                    .trial
                    .as_ref()
                    .and_then(|id| self.trials.get(id))
                    .and_then(|trial| trial.end.as_ref()),
            ) else {
                continue;
            };
            let profile =
                CoreProfile::of_ccd(exposure.observed.profile.ccd(), &self.machine.cores)?;
            let id = EventId::Requalification(night.0);
            observations.push(Observation {
                id: id.clone(),
                profile,
                witness: Witness::FailedRequalification,
            });
            sources.push(Source {
                id,
                seq: exposure.seq,
                time: ended.time,
            });
        }
        let answers: Vec<Answer> = self
            .days
            .answers
            .iter()
            .map(|answer| Answer {
                boot: answer.boot.clone(),
                kind: answer.answer,
            })
            .collect();
        let events = daily_events(&observations, &answers);
        let mut meta: HashMap<EventId, EventMeta> = HashMap::new();
        for event in &events {
            let mut witnesses: Vec<Seq> = sources
                .iter()
                .filter(|source| source.id == event.id)
                .map(|source| source.seq)
                .collect();
            let boot = match &event.id {
                EventId::Boot(boot) => {
                    witnesses.extend(
                        self.days
                            .mces
                            .iter()
                            .filter(|record| &record.boot == boot)
                            .map(|record| record.seq),
                    );
                    witnesses.extend(
                        self.days
                            .live_mces
                            .iter()
                            .filter(|record| &record.boot == boot)
                            .map(|record| record.seq),
                    );
                    Some(boot.clone())
                }
                EventId::Unobserved(_) | EventId::Requalification(_) | EventId::Imported(_) => None,
            };
            witnesses.sort();
            witnesses.dedup();
            let times: Vec<Timestamp> = sources
                .iter()
                .filter(|source| source.id == event.id)
                .map(|source| source.time)
                .collect();
            let first_record = boot
                .as_ref()
                .and_then(|boot| self.days.boots.get(boot))
                .and_then(|records| records.records.first())
                .map(|record| record.time);
            let (Some(&earliest), Some(&latest)) = (times.iter().min(), times.iter().max()) else {
                continue;
            };
            meta.insert(
                event.id.clone(),
                EventMeta {
                    boot,
                    from: first_record.map_or(earliest, |first| first.min(earliest)),
                    to: latest,
                    witnesses,
                },
            );
        }
        Ok(DayView { events, meta })
    }

    /// The closed reproductions, by the day policy's event identity.
    pub(super) fn reproduction_outcomes(&self) -> BTreeMap<EventId, ReproductionOutcome> {
        let mut outcomes = BTreeMap::new();
        for commit in &self.nights.reproductions {
            let Some(id) = self.policy_event_id(&commit.payload.source) else {
                continue;
            };
            match commit.payload.state {
                ReproductionState::Reproduced => {
                    outcomes.insert(id, ReproductionOutcome::Reproduced);
                }
                ReproductionState::Unreproduced => {
                    outcomes.insert(id, ReproductionOutcome::Unreproduced);
                }
                ReproductionState::Untestable => {
                    outcomes.insert(id, ReproductionOutcome::Untestable);
                }
                ReproductionState::Pending => {}
            }
        }
        outcomes
    }

    /// Each event's retreat and reproduction state; none before the day baseline.
    pub(super) fn statuses(&self, view: &DayView) -> Result<Option<Vec<EventStatus>>, NotReady> {
        let Some(history) = self.days.history.as_ref() else {
            return Ok(None);
        };
        Ok(Some(event_statuses(
            &self.machine.cores,
            history,
            &view.events,
            &self.reproduction_outcomes(),
        )?))
    }

    /// The journal's view of a profile an event or interval ran at.
    pub(super) fn observed_profile(&self, profile: &CoreProfile) -> ObservedProfile {
        let offsets = *profile.offsets();
        let uniform: Option<CcdProfile> = self.machine.topology.as_ref().and_then(|topology| {
            let mut ccds = Vec::new();
            for (ccd, cores) in topology.ccds() {
                let mut at = cores.iter().filter_map(|core| offsets.get(core));
                let first = at.next()?;
                if !at.all(|offset| offset == first) {
                    return None;
                }
                ccds.push((*ccd, TargetOffset::new(first.0).ok()?));
            }
            let mut profile = CcdProfile::zero(ccds.iter().map(|&(ccd, _)| ccd)).ok()?;
            for (ccd, offset) in ccds {
                profile = profile.with(ccd, offset);
            }
            Some(profile)
        });
        match uniform {
            Some(ccd) => ObservedProfile::Ccd {
                ccd,
                cores: offsets,
            },
            None => ObservedProfile::PerCore { cores: offsets },
        }
    }

    /// The step back the day policy would require now, as a same-boot invocation sees it: only promotions
    /// wait for a cold boot.
    pub(super) fn pending_retreat(
        &self,
        view: &DayView,
        now: Timestamp,
    ) -> Result<Option<crate::climb::Retreat>, NotReady> {
        if self.climb.history.is_none() {
            return Ok(None);
        }
        Ok(
            match self.climb_decision(view, now, Invocation::SameBoot)?.action {
                crate::climb::ClimbAction::Retreat(retreat) => Some(retreat),
                crate::climb::ClimbAction::Hold { .. }
                | crate::climb::ClimbAction::Promote(_)
                | crate::climb::ClimbAction::Restore(_)
                | crate::climb::ClimbAction::FirmwareZero { .. } => None,
            },
        )
    }

    /// The decision that applied the promotion with this application seq.
    fn decision_of_application(&self, application: Seq) -> Option<DecisionId> {
        self.days
            .applications
            .iter()
            .find(|applied| applied.seq == application)
            .and_then(|applied| match applied.payload.cause {
                togi_journal::ApplyCause::Step { decision } => Some(decision),
                togi_journal::ApplyCause::Reapplication {}
                | togi_journal::ApplyCause::Reset { .. } => None,
            })
    }

    /// The `consumed`, escalation and cursors the pending retreat gives each event it answers.
    fn retreat_marks(
        &self,
        plan: &crate::climb::Retreat,
    ) -> HashMap<EventId, (Escalation, Vec<DecisionId>, Vec<CcdCursor>)> {
        let mut consumed: Vec<_> = plan
            .cursors
            .iter()
            .filter_map(|cursor| Seq::new(cursor.transition.application.get()))
            .filter_map(|application| self.decision_of_application(application))
            .collect();
        consumed.sort_by_key(|decision| decision.0);
        consumed.dedup();
        let cursors = self
            .ccds()
            .into_iter()
            .map(|ccd| CcdCursor {
                ccd,
                depth: super::ledger::saturating_u32(
                    u64::try_from(
                        plan.cursors
                            .iter()
                            .filter(|cursor| cursor.unit == UnitId::Ccd(ccd))
                            .count(),
                    )
                    .unwrap_or(u64::MAX),
                ),
            })
            .collect::<Vec<_>>();
        let strike = self
            .climb
            .history
            .as_ref()
            .map_or(0, |history| history.retreats().len());
        let escalation = if strike == 0 {
            consumed
                .first()
                .map_or(Escalation::EveryCcd { strike: 0 }, |&reverted| {
                    Escalation::RevertNewest { reverted }
                })
        } else {
            Escalation::EveryCcd {
                strike: super::ledger::saturating_u32(u64::try_from(strike).unwrap_or(u64::MAX)),
            }
        };
        plan.events
            .iter()
            .map(|event| {
                (
                    event.clone(),
                    (escalation, consumed.clone(), cursors.clone()),
                )
            })
            .collect()
    }

    /// The application seq of the retreat that answered an event.
    fn retreat_application(&self, id: &EventId) -> Option<Seq> {
        let history = self.days.history.as_ref()?;
        history
            .iter()
            .rev()
            .find(|application| match &application.kind {
                ApplicationKind::Retreat { events, .. }
                | ApplicationKind::Fallback { events, .. } => events.contains(id),
                ApplicationKind::Baseline
                | ApplicationKind::Promotion
                | ApplicationKind::SafetyCap
                | ApplicationKind::FirmwareZero
                | ApplicationKind::Reapplication => false,
            })
            .map(|application| application.at)
    }

    pub(super) fn journal_classification(class: Classification) -> JournalClassification {
        match class {
            Classification::CpuSuspect(_) => JournalClassification::CpuSuspect {
                certainty: Certainty::Definite,
            },
            Classification::ProvisionalCpuSuspect(_) => JournalClassification::CpuSuspect {
                certainty: Certainty::Provisional,
            },
            Classification::NonCpu | Classification::Platform => JournalClassification::NonCpu {},
            Classification::PendingQuestion => JournalClassification::Unexplained {},
        }
    }

    fn describe(class: Classification) -> &'static str {
        match class {
            Classification::CpuSuspect(_) => "CPU-suspect on definite evidence",
            Classification::ProvisionalCpuSuspect(_) => {
                "unexplained end at a nonzero profile: provisionally CPU-suspect"
            }
            Classification::NonCpu => "explained by another cause",
            Classification::Platform => "at CO 0: platform evidence",
            Classification::PendingQuestion => "unexplained end whose question is unanswered",
        }
    }

    /// The `day.event` owed because an event, its resolution or its reproduction is not committed as the
    /// state derives it.
    pub(super) fn owed_day_events(&self, now: Timestamp) -> Result<Vec<Decision>, NotReady> {
        let view = self.day_view()?;
        let statuses = self.statuses(&view)?;
        let marks = match self.pending_retreat(&view, now)? {
            Some(plan) => self.retreat_marks(&plan),
            None => HashMap::new(),
        };
        let mut owed = Vec::new();
        for event in &view.events {
            let (Some(id), Some(meta)) =
                (self.journal_event_id(&event.id), view.meta.get(&event.id))
            else {
                continue;
            };
            let committed = self.days.events.get(&id);
            let status = statuses
                .as_ref()
                .and_then(|statuses| statuses.iter().find(|status| status.id == event.id));
            let resolution = if status.is_some_and(EventStatus::is_resolved) {
                Resolution::Resolved {
                    retreat: self.retreat_application(&event.id),
                }
            } else {
                Resolution::Unresolved {}
            };
            let reproduction = self
                .nights
                .reproductions
                .iter()
                .rev()
                .find(|commit| commit.payload.source == id)
                .map_or(ReproductionState::Pending, |commit| commit.payload.state);
            let kept = committed.map(|book| &book.latest.payload);
            let (escalation, consumed, cursors) = match marks.get(&event.id) {
                Some((escalation, consumed, cursors)) => {
                    (Some(*escalation), consumed.clone(), cursors.clone())
                }
                None => kept.map_or((None, Vec::new(), Vec::new()), |kept| {
                    (kept.escalation, kept.consumed.clone(), kept.cursors.clone())
                }),
            };
            let desired = DayEvent {
                event: id.clone(),
                profile: self.observed_profile(&event.profile),
                boot: meta.boot.clone(),
                from: meta.from,
                to: meta.to,
                witnesses: meta.witnesses.clone(),
                classification: Self::journal_classification(event.class),
                resolution,
                reason: Self::describe(event.class).to_owned(),
                predecessor: kept.and_then(|kept| kept.predecessor.clone()),
                escalation,
                cursors,
                consumed,
                reproduction,
            };
            if kept == Some(&desired) {
                continue;
            }
            let causes = meta.witnesses.clone();
            owed.push(Decision::new(PolicyPayload::DayEvent(desired), causes));
        }
        Ok(owed)
    }

    /// The `day.veto` owed because a CPU-suspect event's veto is not committed active, or a reclassified
    /// event's veto is not committed cleared.
    pub(super) fn owed_day_vetoes(&self) -> Result<Vec<Decision>, NotReady> {
        let view = self.day_view()?;
        let active = vetoes(&view.events);
        let mut owed = Vec::new();
        for veto in &active {
            let (Some(id), Some(meta)) = (
                self.journal_event_id(&veto.event),
                view.meta.get(&veto.event),
            ) else {
                continue;
            };
            let desired = DayVeto {
                event: id.clone(),
                profile: self.observed_profile(&veto.profile),
                state: VetoState::Active,
                evidence: meta.witnesses.clone(),
                reason: match veto.certainty {
                    DayCertainty::Definite => "CPU-suspect event vetoes its profile".to_owned(),
                    DayCertainty::Provisional => "provisional event vetoes its profile".to_owned(),
                },
            };
            if self.days.vetoes.get(&id).map(|commit| &commit.payload) != Some(&desired) {
                owed.push(Decision::new(
                    PolicyPayload::DayVeto(desired),
                    self.days
                        .events
                        .get(&id)
                        .map(|book| book.latest.seq)
                        .into_iter()
                        .collect(),
                ));
            }
        }
        let mut cleared: Vec<_> = self
            .days
            .vetoes
            .iter()
            .filter(|(id, commit)| {
                commit.payload.state == VetoState::Active
                    && !active
                        .iter()
                        .any(|veto| self.journal_event_id(&veto.event).as_ref() == Some(*id))
            })
            .collect();
        cleared.sort_by_key(|(_, commit)| commit.seq);
        owed.extend(cleared.into_iter().map(|(id, commit)| {
            let mut payload = commit.payload.clone();
            payload.state = VetoState::Cleared;
            "its event was reclassified".clone_into(&mut payload.reason);
            Decision::new(
                PolicyPayload::DayVeto(payload),
                self.days
                    .events
                    .get(id)
                    .map(|book| book.latest.seq)
                    .into_iter()
                    .collect(),
            )
        }));
        Ok(owed)
    }

    /// Whether the firmware context differs from the one D was last set under.
    pub(super) fn firmware_state(&self) -> crate::day::Firmware {
        let nonzero = self
            .days
            .history
            .as_ref()
            .is_some_and(|history| !history.current().profile.is_zero());
        if nonzero && self.machine.anchor != self.machine.firmware {
            crate::day::Firmware::Changed
        } else {
            crate::day::Firmware::Current
        }
    }
}
