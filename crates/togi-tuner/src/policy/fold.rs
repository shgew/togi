//! Folding journal events into the policy state. Raw facts and the policy's own commitments fold;
//! historical per-core kinds and every `*.carried` kind never reach the policy.

use crate::{UnitId, UnitProfile};
use togi_journal::{
    Activity, ApplyCause, CrashedBoot, DayApplied, DayEvent, DayEventId, DayFreeze, DayImported,
    DayStep, DeadEndCondition, Event, EventKind, FreezeChoice, Outcome, Payload, Provenance, Queue,
    RetreatCause, Seq, StepKind, Tested, TrialEnd, TrialIntent,
};
use togi_machine::{BootId, CcdProfile, CcdProfileError, Regime};

use super::books::{
    Answered, Committed, DayMce, EventBook, Exposure, LiveMce, ManualRecord, Observed, PlanBook,
    Record, Trial, Unclean,
};
use super::{State, Stop};
use crate::Topology;
use crate::day::{
    ApplicationKind, DayApplication, DayHistory, EventId, Freeze, HistoryError, Reverted,
};

/// A journal the policy cannot fold.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FoldError {
    #[error("recording a day application")]
    History(#[from] HistoryError),
    #[error("building a profile from a journal record")]
    Profile(#[from] CcdProfileError),
    #[error("recording a bounded-climb application")]
    Climb(#[from] crate::climb::ClimbError),
    #[error("invalid climb record: {0}")]
    ClimbRecord(&'static str),
}

impl State {
    /// Folds one event. An event at or before the last folded position is ignored, so a replay of the
    /// whole journal over a live state changes nothing; each exposure additionally counts once per
    /// original identity.
    pub fn fold(&mut self, event: &Event) -> Result<(), FoldError> {
        if self.last.is_some_and(|last| event.seq <= last) {
            return Ok(());
        }
        self.apply(event)?;
        if matches!(
            event.payload,
            Payload::DayUncleanEnd(_)
                | Payload::DayAnswer(_)
                | Payload::DayRecord(_)
                | Payload::DayFault(_)
                | Payload::Mce(_)
                | Payload::ExposureObserved(_)
                | Payload::NightQualification(_)
                | Payload::DayImported(_)
        ) {
            self.observe_raw_campaign(event.time)?;
        }
        self.last = Some(event.seq);
        self.last_time = self.last_time.max(Some(event.time));
        Ok(())
    }

    fn apply(&mut self, event: &Event) -> Result<(), FoldError> {
        let seq = event.seq;
        let paused = self.days.raw_intervals.checkpoint_paused(&event.boot);
        self.days.raw_intervals.apply(event);
        match &event.payload {
            Payload::SessionStart(p) => {
                // A new session starts afresh: no night carries across sessions.
                if self
                    .machine
                    .session
                    .is_some_and(|session| session != p.session)
                {
                    self.close_night(seq);
                }
                self.machine.session = Some(p.session);
                self.machine.cores.clone_from(&p.cores);
                self.machine.topology = Topology::new(&p.cores).ok();
            }
            Payload::SessionContext(p) => self.machine.set_firmware(seq, p.bios_context()),
            Payload::SessionBaseline(_) => self.baseline(seq)?,
            Payload::TrialIntent(p) => self.intent(event, p),
            Payload::TrialStart(p) => {
                if let Some(trial) = self.trials.get_mut(&p.trial)
                    && trial.start.is_none()
                {
                    trial.start = Some((seq, p.clone()));
                }
            }
            Payload::TrialEnd(p) => self.trial_end(event, p),
            Payload::Failure(p) => {
                if let Some(trial) = p.trial.as_ref().and_then(|id| self.trials.get_mut(id))
                    && trial.failure.is_none()
                {
                    trial.failure = Some(seq);
                    if let Some(&at) = p.trial.as_ref().and_then(|id| self.trials.index.get(id)) {
                        self.trials.unreported_failures.remove(&at);
                    }
                }
            }
            Payload::TunerWarning(p) => {
                if let Some(trial) = p.trial.as_ref().and_then(|id| self.trials.get_mut(id)) {
                    trial.warned = true;
                }
                if let Some(&at) = p.trial.as_ref().and_then(|id| self.trials.index.get(id)) {
                    self.trials.unwarned.remove(&at);
                }
            }
            Payload::Mce(p) => {
                if p.trial.is_none() {
                    self.days.mces.push(DayMce {
                        seq,
                        time: event.time,
                        boot: p.from_boot.clone().unwrap_or_else(|| event.boot.clone()),
                        mce: p.clone(),
                    });
                }
            }
            Payload::DayBootStart(p) => {
                // A normal boot after the night's own closes it.
                if self
                    .open_night_boot()
                    .is_some_and(|boot| *boot != event.boot)
                {
                    self.close_night(seq);
                }
                self.machine.set_firmware(seq, p.bios.clone());
                self.record(event, Observed::Start { offsets: p.offsets });
            }
            Payload::DayCheckpoint(p) => self.record(
                event,
                Observed::Checkpoint {
                    applied_secs: p.applied_secs,
                    paused,
                },
            ),
            Payload::DayOffsetsChanged(p) => {
                self.record(event, Observed::Change { offsets: p.offsets });
            }
            Payload::DayCleanStop(_) => self.record(event, Observed::CleanStop),
            Payload::DayUncleanEnd(p) => self.unclean(seq, event.time, p.clone()),
            Payload::DayAnswer(p) => self.answer(seq, p),
            Payload::DayRecord(p) => {
                let origin = self
                    .machine
                    .session
                    .map(|session| Provenance { session, seq });
                self.manual(seq, event.time, origin, p.clone());
            }
            Payload::DayFault(p) => self.live_fault(seq, event.time, event.boot.clone(), p.clone()),
            Payload::ExposureObserved(p) => self.exposure(seq, p),
            Payload::CcdEdge(p) => {
                self.nights.edges.insert(p.ccd, committed(event, p.clone()));
            }
            Payload::CcdCap(p) => {
                self.nights.caps.insert(p.ccd, committed(event, p.clone()));
            }
            Payload::UnitEdge(p) => {
                self.nights
                    .unit_edges
                    .insert(p.unit, committed(event, p.clone()));
                self.observe_finishes(event.time);
            }
            Payload::UnitMapping(p) => self.fold_mapping(event, p)?,
            Payload::ClimbDecision(p) => self.fold_climb_decision(event, p)?,
            Payload::UnitStop(p) => self.fold_unit_stop(event, p)?,
            Payload::CampaignStart(p) => self.fold_campaign_start(event, p)?,
            Payload::CampaignCharge(p) => self.fold_campaign_charge(event, p),
            Payload::CampaignReclassified(p) => self.fold_campaign_reclassified(event, p),
            Payload::FirmwareRestoration(p) => {
                self.climb.restoration = Some(committed(event, p.clone()));
            }
            Payload::SuccessorImported(p) => {
                self.climb.successor = Some(committed(event, p.clone()));
            }
            Payload::SessionImported(p) => {
                self.climb.import_summary = Some(p.clone());
            }
            // A completed stop is the durable record that tuning was left: the operator's (signal,
            // F10, the power button) or the dead end's (`docs/spec/runtime.md`, Night declaration).
            Payload::Shutdown(p) => {
                if matches!(
                    p.reason,
                    togi_journal::ShutdownReason::Signal | togi_journal::ShutdownReason::DeadEnd
                ) {
                    self.close_night(seq);
                }
            }
            Payload::CommandReset(p) => {
                if p.target == togi_journal::ResetTarget::All {
                    self.close_night(seq);
                }
            }
            Payload::NightPlan(p) => {
                let latest = committed(event, p.clone());
                match p.night {
                    // A plan without a night opens one, identified by this plan's sequence.
                    None => {
                        let night = togi_journal::NightId(seq);
                        let ordinal = super::ledger::saturating_u32(
                            u64::try_from(self.nights.plans.len()).unwrap_or(u64::MAX),
                        );
                        self.nights.open = Some(night);
                        self.nights.plans.insert(
                            night,
                            PlanBook {
                                first: latest.clone(),
                                latest,
                                opened_in: event.boot.clone(),
                                ordinal,
                                closed_by: None,
                            },
                        );
                    }
                    Some(night) => {
                        if let Some(book) = self.nights.plans.get_mut(&night) {
                            book.latest = latest;
                        }
                    }
                }
            }
            Payload::NightQualification(p) => {
                for content in &p.contents {
                    self.machine
                        .set_content(seq, content.workload.runs(), &content.content);
                }
                self.nights.qualifications.push(committed(event, p.clone()));
            }
            Payload::NightReproduction(p) => {
                self.nights.reproductions.push(committed(event, p.clone()));
            }
            Payload::DayEvent(p) => {
                self.day_event(event, p);
                self.observe_campaign_event(p);
                self.observe_finishes(event.time);
            }
            Payload::DayVeto(p) => {
                self.days
                    .vetoes
                    .insert(p.event.clone(), committed(event, p.clone()));
            }
            Payload::DayInterval(p) => self.days.intervals.push(committed(event, p.clone())),
            Payload::DayHold(p) => self.days.holds.push(committed(event, p.clone())),
            Payload::DayStep(p) => {
                self.days
                    .steps
                    .insert(p.decision.0, committed(event, p.clone()));
            }
            Payload::DayApplied(p) => self.applied(event, p)?,
            Payload::DayFreeze(p) => self.freeze(p),
            Payload::DayImported(p) => {
                if !self
                    .climb
                    .imports
                    .iter()
                    .any(|known| known.original == p.original)
                {
                    self.climb.imports.push(p.clone());
                    self.climb.import_sequences.insert(p.original, seq);
                }
                self.imported(seq, p);
            }
            Payload::DeadEnd(p) => {
                if p.condition == DeadEndCondition::FailureAtZero && self.stop.is_none() {
                    self.stop = Some(Stop {});
                }
            }
            // Raw facts and commitments the policy does not read: session notices and warnings,
            // archives and the imports' own summary (their day records fold through `day.imported`),
            // configuration, preflight and SMU traffic, profile changes, trial detail, crash
            // detection (its recovered `trial.end` carries the exposure), the historical per-core
            // decisions and every `*.carried` kind (Ruleset 13 starts fresh), and shutdown records.
            Payload::SessionNotice(_)
            | Payload::DayPause(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialCarried(_)
            | Payload::FailureCarried(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::BackendRetry(_)
            | Payload::TaskStart(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => {}
        }
        Ok(())
    }

    /// The verified all-zero baseline starts the day history.
    fn baseline(&mut self, seq: Seq) -> Result<(), FoldError> {
        let Some(topology) = &self.machine.topology else {
            return Ok(());
        };
        let zero = CcdProfile::zero(topology.ccds().iter().map(|&(ccd, _)| ccd))?;
        self.days.history = Some(DayHistory::baseline(seq, UnitProfile::of_ccd(&zero)));
        if self.climb.history.is_none() {
            self.climb.history = Some(crate::climb::ClimbHistory::stock(crate::Units::coarse(
                topology,
            )));
        }
        self.machine.anchor.clone_from(&self.machine.firmware);
        Ok(())
    }

    /// A Ruleset 13 trial carries its night, task, tested scope and a Ruleset 13 activity; any other
    /// trial belongs to the old per-core policy.
    fn intent(&mut self, event: &Event, intent: &TrialIntent) {
        let r13 = matches!(
            intent.activity,
            Some(
                Activity::Mapping
                    | Activity::Qualification
                    | Activity::Reproduction
                    | Activity::Control
            )
        ) && intent.night.is_some()
            && intent.task.is_some()
            && intent.tested.is_some();
        if !r13 || self.trials.index.contains_key(&intent.trial) {
            return;
        }
        for content in &intent.content {
            self.machine
                .set_content(event.seq, content.workload.runs(), &content.content);
        }
        let at = self.trials.all.len();
        self.trials.index.insert(intent.trial.clone(), at);
        if let Some(night) = intent.night {
            self.trials.by_night.entry(night).or_default().push(at);
        }
        if let Some(task) = intent.task {
            self.trials.by_task.entry(task).or_default().push(at);
            if task.queue == Queue::Filler {
                self.trials.fillers.push(at);
            }
        }
        self.trials.all.push(Trial {
            intent_seq: event.seq,
            intent_time: event.time,
            boot: event.boot.clone(),
            firmware: self.machine.firmware.clone(),
            intent: intent.clone(),
            start: None,
            end: None,
            failure: None,
            warned: false,
        });
    }

    fn trial_end(&mut self, event: &Event, end: &TrialEnd) {
        let Some(&at) = self.trials.index.get(&end.trial) else {
            return;
        };
        let Some(trial) = self.trials.all.get_mut(at) else {
            return;
        };
        if trial.end.is_some() {
            return;
        }
        trial.end = Some(super::books::Ended {
            seq: event.seq,
            time: event.time,
            end: end.clone(),
        });
        let failed = end.outcome == Outcome::Failure;
        let isolated = matches!(trial.intent.tested, Some(Tested::Isolated { .. }));
        let load = trial.intent.regime == Regime::R7 && !end.voltage_requests_v.is_empty();
        if failed {
            if trial.failure.is_none() {
                self.trials.unreported_failures.insert(at);
            }
            if isolated && !trial.warned {
                self.trials.unwarned.insert(at);
            }
        }
        self.trials.ended_order.push(at);
        if load {
            self.trials.loads.push(at);
        }
        if self
            .machine
            .session
            .is_none_or(|session| !self.ledger.seen.contains(&(session, trial.intent_seq)))
        {
            self.trials.unobserved.insert(at);
        }
    }

    fn record(&mut self, event: &Event, what: Observed) {
        self.days
            .boots
            .entry(event.boot.clone())
            .or_default()
            .records
            .push(Record {
                seq: event.seq,
                time: event.time,
                mono_ns: event
                    .mono
                    .and_then(|mono| u64::try_from(mono.as_nanos()).ok()),
                what,
            });
    }

    fn unclean(&mut self, seq: Seq, time: jiff::Timestamp, end: togi_journal::DayUncleanEnd) {
        if self
            .days
            .unclean
            .iter()
            .any(|known| known.end.boot == end.boot)
        {
            return;
        }
        self.days.unclean.push(Unclean { seq, time, end });
    }

    /// A manual crash record: the first record of a crash identity counts, and keeps its provenance.
    fn manual(
        &mut self,
        seq: Seq,
        time: jiff::Timestamp,
        origin: Option<Provenance>,
        record: togi_journal::DayRecord,
    ) {
        if self
            .days
            .manual
            .iter()
            .any(|known| known.record.boot == record.boot)
        {
            return;
        }
        self.days.manual.push(ManualRecord {
            seq,
            time,
            origin,
            record,
        });
    }

    /// A live fault: only a machine check can be CPU evidence (`docs/spec/tuner.md`, Live faults and
    /// load mix); kernel errors and traps stay raw journal evidence. One per boot and fault identity,
    /// whether observed or imported.
    fn live_fault(
        &mut self,
        seq: Seq,
        time: jiff::Timestamp,
        boot: BootId,
        fault: togi_journal::DayFault,
    ) {
        if !matches!(fault.fault, togi_journal::LiveFault::MachineCheck { .. }) {
            return;
        }
        let identity = fault.identity();
        if self
            .days
            .live_mces
            .iter()
            .any(|known| known.boot == boot && known.fault.identity() == identity)
        {
            return;
        }
        self.days.live_mces.push(LiveMce {
            seq,
            time,
            boot,
            fault,
        });
    }

    /// An answer counts only when it names a recorded unclean end's boot and that end's offsets.
    fn answer(&mut self, seq: Seq, answer: &togi_journal::DayAnswer) {
        let known = self
            .days
            .unclean
            .iter()
            .any(|known| known.end.boot == answer.boot && known.end.offsets == answer.offsets);
        if known {
            self.days.answers.push(Answered {
                seq,
                boot: answer.boot.clone(),
                answer: answer.answer,
            });
        }
    }

    fn exposure(&mut self, seq: Seq, observed: &togi_journal::ExposureObserved) {
        let identity = (observed.source.session, observed.source.seq);
        if !self.ledger.seen.insert(identity) {
            return;
        }
        let trial = observed
            .source
            .trial
            .as_ref()
            .and_then(|id| self.trials.get(id));
        let task = trial.and_then(Trial::task);
        let night = trial.and_then(Trial::night);
        let index = self.ledger.records.len();
        self.ledger.records.push(Exposure {
            seq,
            observed: observed.clone(),
            task,
            night,
        });
        if let Some(task) = task {
            self.ledger.by_task.entry(task).or_default().push(index);
        }
        if Some(observed.source.session) == self.machine.session
            && let Some(&at) = observed
                .source
                .trial
                .as_ref()
                .and_then(|id| self.trials.index.get(id))
        {
            self.trials.unobserved.remove(&at);
        }
        self.aggregate(index, seq, observed);
    }

    fn day_event(&mut self, event: &Event, payload: &DayEvent) {
        let commit = committed(event, payload.clone());
        self.days.event_commits.insert(event.seq, payload.clone());
        match self.days.events.get_mut(&payload.event) {
            Some(book) => {
                if book.latest.payload.classification != payload.classification {
                    self.days.reclassified = Some(event.seq);
                }
                book.latest = commit;
            }
            None => {
                self.days.events.insert(
                    payload.event.clone(),
                    EventBook {
                        first: event.seq,
                        latest: commit,
                    },
                );
            }
        }
    }

    fn freeze(&mut self, freeze: &DayFreeze) {
        self.days.freeze = match freeze.choice {
            FreezeChoice::Freeze => Freeze::On,
            FreezeChoice::Unfreeze => Freeze::Off,
        };
    }

    /// An imported day record folds as the raw record it carries; one that does not decode, or whose
    /// kind is not a raw day observation, stays unfolded rather than becoming guessed evidence.
    fn imported(&mut self, seq: Seq, imported: &DayImported) {
        let identity = (imported.original.session, imported.original.seq);
        if !self.days.imported.insert(identity) {
            return;
        }
        let kind = imported.original_kind;
        if kind == EventKind::DayUncleanEnd {
            if let Ok(end) = serde_json::from_str::<togi_journal::DayUncleanEnd>(&imported.raw) {
                self.unclean(seq, imported.time, end);
            }
        } else if kind == EventKind::DayAnswer {
            if let Ok(answer) = serde_json::from_str::<togi_journal::DayAnswer>(&imported.raw) {
                self.answer(seq, &answer);
            }
        } else if kind == EventKind::DayRecord {
            if let Ok(record) = serde_json::from_str::<togi_journal::DayRecord>(&imported.raw) {
                self.manual(seq, imported.time, Some(imported.original), record);
            }
        } else if kind == EventKind::DayFault {
            if let Ok(fault) = serde_json::from_str::<togi_journal::DayFault>(&imported.raw) {
                self.live_fault(seq, imported.time, imported.boot.clone(), fault);
            }
        } else if kind == EventKind::Mce
            && let Ok(mce) = serde_json::from_str::<togi_journal::Mce>(&imported.raw)
            && mce.trial.is_none()
        {
            self.days.mces.push(DayMce {
                seq,
                time: imported.time,
                boot: mce
                    .from_boot
                    .clone()
                    .unwrap_or_else(|| imported.boot.clone()),
                mce,
            });
        }
    }

    /// A successful application extends the day history. Its kind comes from the step it carried out.
    fn applied(&mut self, event: &Event, applied: &DayApplied) -> Result<(), FoldError> {
        self.fold_climb_application(event, applied)?;
        let kind = self.application_kind(applied);
        let zeroed = kind == ApplicationKind::FirmwareZero;
        self.days
            .applications
            .push(committed(event, applied.clone()));
        if let Some(history) = &mut self.days.history {
            history.push(DayApplication {
                at: event.seq,
                profile: UnitProfile::of_ccd(applied.target.ccd()),
                kind,
            })?;
        }
        if zeroed {
            self.machine.anchor.clone_from(&self.machine.firmware);
        }
        Ok(())
    }

    fn application_kind(&self, applied: &DayApplied) -> ApplicationKind {
        if let Some(kind) = self.climb_application_kind(applied) {
            return kind;
        }
        let step = match applied.cause {
            ApplyCause::Reapplication {} => return ApplicationKind::Reapplication,
            // A reset starts a new hold and consumes nothing, as a cap retreat does.
            ApplyCause::Reset { .. } => return ApplicationKind::SafetyCap,
            ApplyCause::Step { decision } => self.days.steps.get(&decision.0),
        };
        let Some(step) = step else {
            return ApplicationKind::SafetyCap;
        };
        match &step.payload.step {
            StepKind::Promote { .. } => ApplicationKind::Promotion,
            StepKind::FirmwareZero { .. } => ApplicationKind::FirmwareZero,
            StepKind::Hold { .. } => ApplicationKind::Reapplication,
            StepKind::Retreat { cause, wanted, .. } => {
                let events = self.cited_events(&step.payload);
                let reverted = match cause {
                    RetreatCause::Completion {} | RetreatCause::ShallowerCap {} => Vec::new(),
                    RetreatCause::Event { .. } => self.reverted(&step.payload),
                };
                match (cause, wanted) {
                    (RetreatCause::ShallowerCap {}, None) => ApplicationKind::SafetyCap,
                    (_, Some(wanted)) => ApplicationKind::Fallback {
                        events,
                        reverted,
                        wanted: UnitProfile::of_ccd(wanted.ccd()),
                    },
                    (RetreatCause::Event { .. } | RetreatCause::Completion {}, None) => {
                        ApplicationKind::Retreat { events, reverted }
                    }
                }
            }
        }
    }

    /// The events a step cites, as the day policy names them.
    fn cited_events(&self, step: &DayStep) -> Vec<EventId> {
        step.causes
            .events
            .iter()
            .filter_map(|seq| self.days.event_commits.get(seq))
            .filter_map(|event| self.policy_event_id(&event.event))
            .collect()
    }

    /// The promotions the cited events consumed: each one CCD's step, by the seq of its application.
    fn reverted(&self, step: &DayStep) -> Vec<Reverted> {
        let mut reverted: Vec<Reverted> = step
            .causes
            .events
            .iter()
            .filter_map(|seq| self.days.event_commits.get(seq))
            .flat_map(|event| event.consumed.iter())
            .filter_map(|decision| self.promotion_of(decision.0))
            .collect();
        reverted.sort();
        reverted.dedup();
        reverted
    }

    /// The application seq and CCD of the promotion decided as `decision`.
    pub(super) fn promotion_of(&self, decision: u64) -> Option<Reverted> {
        let step = self.days.steps.get(&decision)?;
        let StepKind::Promote { changed } = step.payload.step else {
            return None;
        };
        let application = self.days.applications.iter().find(|application| {
            matches!(application.payload.cause, ApplyCause::Step { decision: id } if id.0 == decision)
        })?;
        Some(Reverted {
            step: application.seq,
            unit: UnitId::Ccd(changed),
        })
    }
}

impl State {
    /// The day policy's name for a journal event identity. A manual record's identity is its crash's: the
    /// crashed boot or crash instant; a receipt without its raw record keeps its original namespace.
    pub(super) fn policy_event_id(&self, id: &DayEventId) -> Option<EventId> {
        match id {
            DayEventId::Boot { boot } => Some(EventId::Boot(boot.clone())),
            DayEventId::Requalification { night } => Some(EventId::Requalification(night.0)),
            DayEventId::Record { session, seq } => {
                let origin = Provenance {
                    session: *session,
                    seq: *seq,
                };
                self.days
                    .manual
                    .iter()
                    .find(|manual| manual.origin == Some(origin))
                    .map(|manual| match &manual.record.boot {
                        CrashedBoot::UncleanEnd { boot } => EventId::Boot(boot.clone()),
                        CrashedBoot::Unobserved { at } => EventId::Unobserved(*at),
                    })
                    .or(Some(EventId::Imported(origin)))
            }
        }
    }

    /// The journal's name for the day policy's event identity. A crash no observer saw is named by the
    /// record that reported it; one whose record has no provenance has none.
    pub(super) fn journal_event_id(&self, id: &EventId) -> Option<DayEventId> {
        match id {
            EventId::Boot(boot) => Some(DayEventId::Boot { boot: boot.clone() }),
            EventId::Requalification(at) => Some(DayEventId::Requalification {
                night: togi_journal::NightId(*at),
            }),
            EventId::Imported(origin) => Some(DayEventId::Record {
                session: origin.session,
                seq: origin.seq,
            }),
            EventId::Unobserved(at) => self
                .days
                .manual
                .iter()
                .find(|manual| match &manual.record.boot {
                    CrashedBoot::Unobserved { at: recorded } => recorded == at,
                    CrashedBoot::UncleanEnd { .. } => false,
                })
                .and_then(|manual| manual.origin)
                .map(|origin| DayEventId::Record {
                    session: origin.session,
                    seq: origin.seq,
                }),
        }
    }
}

impl State {
    /// The boot the open night began in, while one is open.
    fn open_night_boot(&self) -> Option<&BootId> {
        let night = self.nights.open?;
        self.nights.plans.get(&night).map(|book| &book.opened_in)
    }

    /// Closes the open night, if any, at `seq`.
    fn close_night(&mut self, seq: Seq) {
        if let Some(night) = self.nights.open.take()
            && let Some(book) = self.nights.plans.get_mut(&night)
        {
            book.closed_by = Some(seq);
        }
    }
}

pub(super) fn committed<T>(event: &Event, payload: T) -> Committed<T> {
    Committed {
        seq: event.seq,
        time: event.time,
        causes: event.cause.clone(),
        payload,
    }
}
