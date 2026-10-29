//! Production D on the successor calendar. The report is a streaming journal projection:
//! neither simulator truth nor the tuner's private state supplies a reporting fact.

use std::collections::BTreeMap;

use jiff::Timestamp;
use togi_journal::{self as journal, Event, Payload, Seq};
use togi_machine::{Machine, PerCore, TargetOffset};

use super::successor::{self as records, PolicyApplications, PolicyReport};
use super::{DayClassification, DayObservation, Night, Policy, Ruleset13, RunError};

/// The production tuner and execution driver, with successor observations enabled.
pub struct SuccessorPolicy {
    driver: Ruleset13,
}

impl SuccessorPolicy {
    /// Builds D at a registered fallback slope.
    pub fn new(multiplier: f64) -> Result<Self, RunError> {
        let mut driver = Ruleset13::new(multiplier)?;
        driver.successor = Some(JournalFold::new());
        Ok(Self { driver })
    }

    /// Retains the source journal for replay evidence, not for ordinary long runs.
    #[must_use]
    pub fn recording(mut self) -> Self {
        self.driver = self.driver.recording();
        self
    }

    #[must_use]
    pub fn events(&self) -> Option<&[Event]> {
        self.driver.events()
    }
}

impl Policy for SuccessorPolicy {
    fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.driver.firmware_changed(machine)
    }

    fn start_day(&mut self, day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.driver.start_day(day, machine)
    }

    fn observe_day(
        &mut self,
        day: u32,
        evidence: &DayObservation,
        machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError> {
        self.driver.observe_day(day, evidence, machine)
    }

    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
        self.driver.night(night, machine)
    }

    fn report(&self) -> PolicyReport {
        self.driver
            .successor
            .as_ref()
            .map_or_else(PolicyReport::unavailable, |fold| fold.report.clone())
    }

    fn applications(&self) -> PolicyApplications {
        self.driver
            .successor
            .as_ref()
            .map_or_else(PolicyApplications::default, |fold| {
                fold.applications.clone()
            })
    }
}

#[derive(Clone)]
struct RecordedDecision {
    proposal: journal::ClimbDecision,
    causes: Vec<Seq>,
    consumed: u64,
}

/// Retains journal witness identities and report facts, never raw trial sample histories.
pub(super) struct JournalFold {
    pub(super) report: PolicyReport,
    pub(super) applications: PolicyApplications,
    origin: Option<Timestamp>,
    decisions: BTreeMap<Seq, RecordedDecision>,
    certificates: BTreeMap<Seq, records::Qualification>,
    exposures: BTreeMap<Seq, String>,
    failure_events: BTreeMap<Seq, String>,
    event_aliases: Vec<(journal::DayEventId, String)>,
    ceilings: BTreeMap<journal::UnitId, u8>,
    mapping: Option<journal::UnitMapping>,
    epoch: String,
    stock_controls: Vec<(togi_machine::BiosContext, u32, String)>,
    restoration: Option<usize>,
    saved_restoration: Option<(Seq, f64, journal::UnitVector)>,
    restoration_backstep: Option<journal::DayEventId>,
    reclassified: BTreeMap<String, f64>,
    retreat_receipts: BTreeMap<Seq, usize>,
    application_days: BTreeMap<journal::ApplicationId, f64>,
}

impl JournalFold {
    pub(super) fn new() -> Self {
        Self {
            report: PolicyReport::unavailable(),
            applications: PolicyApplications::default(),
            origin: None,
            decisions: BTreeMap::new(),
            certificates: BTreeMap::new(),
            exposures: BTreeMap::new(),
            failure_events: BTreeMap::new(),
            event_aliases: Vec::new(),
            ceilings: BTreeMap::new(),
            mapping: None,
            epoch: "initial".to_owned(),
            stock_controls: Vec::new(),
            restoration: None,
            saved_restoration: None,
            restoration_backstep: None,
            reclassified: BTreeMap::new(),
            retreat_receipts: BTreeMap::new(),
            application_days: BTreeMap::new(),
        }
    }

    fn day(&self, time: Timestamp) -> f64 {
        self.origin.map_or(1.0, |origin| {
            1.0 + time.duration_since(origin).as_secs_f64() / 86_400.0
        })
    }

    fn canonical_event(&self, event: &journal::DayEventId) -> String {
        self.event_aliases
            .iter()
            .find(|(alias, _)| alias == event)
            .map_or_else(|| event_id(event), |(_, id)| id.clone())
    }

    fn mapping(&mut self, mapping: &journal::UnitMapping) -> Result<(), RunError> {
        if self.mapping.as_ref() == Some(mapping) {
            return Ok(());
        }
        self.report.unit_membership = membership(mapping)?;
        self.mapping = Some(mapping.clone());
        Ok(())
    }

    pub(super) fn fold(&mut self, event: &Event) -> Result<(), RunError> {
        self.origin.get_or_insert(event.time);
        let day = self.day(event.time);
        match &event.payload {
            Payload::CampaignStart(start) => {
                self.report
                    .allowance
                    .get_or_insert(records::AllowanceLedger {
                        limit: u64::from(start.allowance),
                        consumed: 0,
                        provisional: 0,
                        reclassified: 0,
                        exhausted_day: None,
                    });
            }
            Payload::ClimbDecision(decision) => {
                self.mapping(&decision.before.mapping)?;
                if !matches!(decision.decision, journal::ClimbAction::Hold { .. }) {
                    self.decisions.insert(
                        event.seq,
                        RecordedDecision {
                            proposal: decision.clone(),
                            causes: event.cause.clone(),
                            consumed: self
                                .report
                                .allowance
                                .as_ref()
                                .map_or(0, |ledger| ledger.consumed),
                        },
                    );
                }
            }
            Payload::ExposureObserved(exposure) => {
                if let Some(trial) = &exposure.source.trial {
                    let id = trial.as_str().to_owned();
                    self.exposures.insert(event.seq, id.clone());
                    if exposure.outcome == journal::Outcome::Failure {
                        self.failure_events
                            .insert(event.seq, format!("journal-failure-{}", trial.as_str()));
                    }
                    if exposure.activity == journal::Activity::Control
                        && exposure.outcome == journal::Outcome::Pass
                        && exposure
                            .profile
                            .cores()
                            .iter()
                            .all(|(_, offset)| offset == TargetOffset::ZERO)
                        && exposure.exclusions.is_empty()
                        && exposure.failures.is_empty()
                    {
                        self.stock_controls
                            .push((exposure.firmware.clone(), exposure.epoch, id));
                    }
                }
            }
            Payload::TrialEnd(end) => {
                if end.outcome == journal::Outcome::Failure {
                    let id = format!("journal-failure-{}", end.trial.as_str());
                    self.failure_events.insert(event.seq, id.clone());
                    self.report.events.push(records::LearningEvent {
                        id,
                        day,
                        classification: records::Classification::CpuSuspect,
                        provisional: false,
                        reclassified_from: None,
                        charged: false,
                        phase: if self.finished() {
                            records::Phase::PostFinish
                        } else {
                            records::Phase::Learning
                        },
                        unit_phase: phase(
                            self.mapping
                                .as_ref()
                                .map_or(journal::UnitPhase::Coarse, |mapping| mapping.phase),
                        ),
                        newest_step: self.report.newest_step.clone(),
                    });
                }
            }
            Payload::NightQualification(certificate) => {
                if let journal::QualificationState::Failed { failure, .. } = &certificate.state
                    && let Some(id) = self.failure_events.get(failure).cloned()
                {
                    self.failure_events.insert(event.seq, id);
                }
                if let journal::QualificationState::Complete { witnesses } = &certificate.state {
                    let mut fresh = Vec::new();
                    let mut reused = Vec::new();
                    for witness in witnesses {
                        let (exposure, destination) = match witness.evidence {
                            journal::CellEvidence::Fresh { exposure } => (exposure, &mut fresh),
                            journal::CellEvidence::Reused { exposure } => (exposure, &mut reused),
                        };
                        if let Some(trial) = self.exposures.get(&exposure)
                            && !destination.contains(trial)
                        {
                            destination.push(trial.clone());
                        }
                    }
                    let profile = depths(certificate.profile.cores())?;
                    self.certificates.insert(
                        event.seq,
                        records::Qualification {
                            id: seq_id(event.seq),
                            day,
                            epoch: self.epoch.clone(),
                            profile,
                            // Context and evidence epoch are explicit journal identities, not a
                            // guessed time window around the firmware transition.
                            stock_controls: self
                                .stock_controls
                                .iter()
                                .filter(|(firmware, epoch, _)| {
                                    firmware == &certificate.firmware && *epoch == certificate.epoch
                                })
                                .map(|(_, _, id)| id.clone())
                                .collect(),
                            fresh_witnesses: fresh,
                            reused_witnesses: reused,
                            safety_checks_satisfied: true,
                        },
                    );
                }
            }
            Payload::UnitEdge(edge) => {
                if let Some(ceiling) = edge.ceiling {
                    let depth = super::depth(ceiling)?;
                    self.ceilings.insert(edge.unit, depth);
                    let (observation, witnesses) = match &edge.new {
                        journal::Edge::Unknown {} => return Ok(()),
                        journal::Edge::Observed { offset, witness } => (
                            records::EdgeObservation::Measured {
                                u_depth: super::depth(*offset)?,
                            },
                            vec![seq_id(*witness)],
                        ),
                        journal::Edge::Censored { witnesses, .. } => (
                            records::EdgeObservation::CensoredWriteLimit {},
                            witnesses.iter().copied().map(seq_id).collect(),
                        ),
                    };
                    self.applications.ceilings.push((
                        unit_id(edge.unit)?,
                        records::CeilingEvidence {
                            day,
                            epoch: self.epoch.clone(),
                            edge: observation,
                            ceiling_depth: depth,
                            witnesses,
                        },
                    ));
                }
            }
            Payload::DayApplied(applied) => self.applied(event, applied)?,
            Payload::UnitStop(stop) => {
                self.mapping(&stop.profile.mapping)?;
                let unit = unit_id(stop.unit)?;
                let (reason, triggering) = match &stop.reason {
                    journal::UnitStopReason::Ceiling {} => (records::FinishReason::Ceiling, None),
                    journal::UnitStopReason::FirstBackstep { event, .. } => {
                        (records::FinishReason::CpuSuspectStop, Some(event))
                    }
                    journal::UnitStopReason::AllowanceExhausted {} => {
                        (records::FinishReason::AllowanceExhausted, None)
                    }
                    journal::UnitStopReason::RestorationBackstep { event } => {
                        (records::FinishReason::FirmwareRestorationStop, Some(event))
                    }
                    journal::UnitStopReason::AdverseCeiling {}
                    | journal::UnitStopReason::Veto {} => {
                        (records::FinishReason::CpuSuspectStop, None)
                    }
                    journal::UnitStopReason::Predecessor { .. } => {
                        (records::FinishReason::InheritedStop, None)
                    }
                };
                let mut witnesses: Vec<_> = stop.witnesses.iter().copied().map(seq_id).collect();
                if let Some(triggering) = triggering {
                    witnesses.push(self.canonical_event(triggering));
                }
                let finish = records::UnitFinish {
                    unit,
                    reason,
                    day,
                    witnesses,
                };
                if !self.report.finishes.iter().any(|item| item.unit == unit) {
                    self.report.finishes.push(finish.clone());
                }
                if reason == records::FinishReason::InheritedStop
                    && !self
                        .report
                        .inherited_stops
                        .iter()
                        .any(|item| item.unit == unit)
                {
                    self.report.inherited_stops.push(finish);
                }
                if !self.report.stopped_units.contains(&unit) {
                    self.report.stopped_units.push(unit);
                }
            }
            Payload::DayEvent(observed) => {
                if matches!(observed.event, journal::DayEventId::Requalification { .. })
                    && let Some(id) = observed
                        .witnesses
                        .iter()
                        .find_map(|seq| self.failure_events.get(seq))
                    && !self
                        .event_aliases
                        .iter()
                        .any(|(event, _)| event == &observed.event)
                {
                    self.event_aliases
                        .push((observed.event.clone(), id.clone()));
                }
                let id = self.canonical_event(&observed.event);
                let classification = classification(observed.classification);
                let provisional = matches!(
                    observed.classification,
                    journal::Classification::CpuSuspect {
                        certainty: journal::Certainty::Provisional
                    }
                );
                if let Some(previous) = self.report.events.iter_mut().find(|item| item.id == id) {
                    if previous.classification != classification {
                        previous
                            .reclassified_from
                            .get_or_insert(previous.classification);
                    }
                    previous.classification = classification;
                    previous.provisional = provisional;
                } else {
                    self.report.events.push(records::LearningEvent {
                        id,
                        day: self.day(observed.to),
                        classification,
                        provisional,
                        reclassified_from: None,
                        charged: false,
                        phase: if self.finished() {
                            records::Phase::PostFinish
                        } else {
                            records::Phase::Learning
                        },
                        unit_phase: phase(
                            self.mapping
                                .as_ref()
                                .map_or(journal::UnitPhase::Coarse, |mapping| mapping.phase),
                        ),
                        newest_step: self.report.newest_step.clone(),
                    });
                }
            }
            Payload::CampaignCharge(charge) => {
                self.mapping(&charge.profile.mapping)?;
                let id = self.canonical_event(&charge.event);
                if let Some(item) = self.report.events.iter_mut().find(|item| item.id == id) {
                    item.charged = true;
                    item.provisional = charge.provisional;
                    item.phase = match charge.phase {
                        journal::CampaignEventPhase::Learning => records::Phase::Learning,
                        journal::CampaignEventPhase::PostFinish => records::Phase::PostFinish,
                    };
                    item.unit_phase = phase(charge.unit_phase);
                }
                if let Some(ledger) = &mut self.report.allowance {
                    ledger.consumed = u64::from(charge.consumed);
                    if charge.exhausted {
                        ledger.exhausted_day.get_or_insert(day);
                    }
                }
                self.update_counts();
            }
            Payload::CampaignReclassified(reclassified) => {
                let id = self.canonical_event(&reclassified.event);
                if matches!(reclassified.to, journal::Classification::NonCpu {})
                    && matches!(
                        reclassified.from,
                        journal::Classification::CpuSuspect { .. }
                    )
                {
                    self.reclassified.insert(id.clone(), day);
                } else {
                    self.reclassified.remove(&id);
                }
                if let Some(item) = self.report.events.iter_mut().find(|item| item.id == id) {
                    item.reclassified_from = Some(classification(reclassified.from));
                    item.classification = classification(reclassified.to);
                    item.provisional = false;
                }
                self.update_counts();
            }
            Payload::FirmwareRestoration(restoration) => self.restoration(event, restoration),
            Payload::UnitMapping(mapping) => self.mapping(mapping)?,
            Payload::DayVeto(veto) => self.veto(veto)?,
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionImported(_)
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
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
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
            | Payload::StateRebuilt(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayPause(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::TaskStart(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::SuccessorImported(_) => {}
        }
        Ok(())
    }

    fn finished(&self) -> bool {
        !self.report.unit_membership.is_empty()
            && self.report.unit_membership.iter().all(|unit| {
                self.report
                    .finishes
                    .iter()
                    .any(|finish| finish.unit == unit.unit)
            })
    }

    fn update_counts(&mut self) {
        if let Some(ledger) = &mut self.report.allowance {
            ledger.provisional = self
                .report
                .events
                .iter()
                .filter(|item| item.charged && item.provisional)
                .count() as u64;
            ledger.reclassified = self
                .report
                .events
                .iter()
                .filter(|item| {
                    item.charged
                        && item.reclassified_from.is_some()
                        && item.classification != records::Classification::CpuSuspect
                })
                .count() as u64;
        }
    }

    fn certificate(
        &self,
        decision: &journal::ClimbDecision,
    ) -> Result<records::Qualification, RunError> {
        decision
            .certificate
            .and_then(|seq| self.certificates.get(&seq))
            .cloned()
            .ok_or(RunError::Calendar(
                "applied D step has no journaled complete qualification",
            ))
    }

    fn applied(&mut self, event: &Event, applied: &journal::DayApplied) -> Result<(), RunError> {
        let after = observed_depths(&applied.readbacks)?;
        self.report.readback = Some(after);
        let day = self.day(event.time);
        self.application_days.insert(applied.application, day);
        if after.iter().all(|(_, depth)| depth == 0) {
            if let Some((seq, transition_day, saved)) = self.saved_restoration.take() {
                self.applications
                    .restorations
                    .push(records::FirmwareRestoration {
                        id: seq_id(seq),
                        epoch: self.epoch.clone(),
                        transition_day,
                        previous_profile: depths(&saved.cores)?,
                        membership: membership(&saved.mapping)?,
                        stock_reset_day: day,
                        stock_readback: after,
                        outcome: records::RestorationOutcome::Blocked {
                            reason: "awaiting journaled qualification, stock hold and application"
                                .to_owned(),
                        },
                    });
                self.restoration = Some(self.applications.restorations.len() - 1);
            }
            let pending = self
                .restoration_backstep
                .take()
                .map(|cause| self.canonical_event(&cause));
            if let Some(cause) = pending
                && let Some(index) = self.restoration
                && let records::RestorationOutcome::Applied { result, .. } =
                    &mut self.applications.restorations[index].outcome
            {
                *result = records::RestorationResult::Crashed {
                    event: cause,
                    stock_step_back_day: day,
                    stock_readback: after,
                    stopped_units: self.report.stopped_units.clone(),
                    witnesses: vec![seq_id(event.seq)],
                };
            }
        }
        let journal::ApplyCause::Step { decision } = applied.cause else {
            return Ok(());
        };
        let seq =
            Seq::new(decision.0).ok_or(RunError::Calendar("application decision sequence"))?;
        let recorded = self.decisions.get(&seq).cloned().ok_or(RunError::Calendar(
            "application has no journaled climb decision",
        ))?;
        let proposal = &recorded.proposal;
        self.mapping(&proposal.proposed.mapping)?;
        let day = self.day(event.time);
        let id = seq_id(seq);
        let before = depths(&proposal.before.cores)?;
        let kind = match &proposal.decision {
            journal::ClimbAction::Hold { .. } => {
                return Err(RunError::Calendar("hold recorded as applied step"));
            }
            journal::ClimbAction::Promote { unit, .. } => {
                let unit_id = unit_id(*unit)?;
                self.report.newest_step = Some(records::StepIdentity::Unit {
                    unit: unit_id,
                    proposal: id.clone(),
                });
                records::StepKind::Climb {
                    unit: unit_id,
                    ceiling_depth: *self.ceilings.get(unit).ok_or(RunError::Calendar(
                        "applied promotion has no journaled ceiling",
                    ))?,
                    clean_hold_hours: std::time::Duration::from_secs(proposal.hold.eligible_s)
                        .as_secs_f64()
                        / 3600.0,
                    qualification: self.certificate(proposal)?,
                }
            }
            journal::ClimbAction::Retreat { cause, .. } => records::StepKind::Retreat {
                event: match cause {
                    journal::ClimbRetreatCause::Event { event }
                    | journal::ClimbRetreatCause::Veto { event } => {
                        Some(self.canonical_event(event))
                    }
                    journal::ClimbRetreatCause::AdverseCeiling { .. }
                    | journal::ClimbRetreatCause::Completion {} => None,
                },
                units: self
                    .report
                    .unit_membership
                    .iter()
                    .filter(|unit| {
                        unit.cores.iter().any(|core| {
                            after
                                .get(*core)
                                .zip(before.get(*core))
                                .is_some_and(|(after, before)| after < before)
                        })
                    })
                    .map(|unit| unit.unit)
                    .collect(),
            },
            journal::ClimbAction::FirmwareZero {} => records::StepKind::StockReset {
                epoch: self.epoch.clone(),
            },
            journal::ClimbAction::Restore {
                restoration: journal::RestorationKind::Firmware,
                units,
            } => {
                let index = self.restoration.ok_or(RunError::Calendar(
                    "firmware application has no saved journal profile",
                ))?;
                let qualification = self.certificate(proposal)?;
                let item = &mut self.applications.restorations[index];
                item.outcome = records::RestorationOutcome::Applied {
                    day,
                    qualification,
                    clean_stock_hours: std::time::Duration::from_secs(proposal.hold.eligible_s)
                        .as_secs_f64()
                        / 3600.0,
                    result: records::RestorationResult::Retained {},
                };
                let restoration = item.id.clone();
                self.report.newest_step = Some(records::StepIdentity::Restoration {
                    restoration: restoration.clone(),
                    units: units
                        .iter()
                        .copied()
                        .map(unit_id)
                        .collect::<Result<_, _>>()?,
                });
                records::StepKind::Restoration { restoration }
            }
            journal::ClimbAction::Restore {
                restoration: journal::RestorationKind::Reclassified,
                ..
            } => self.reclassified_restoration(&recorded, seq, before, after)?,
        };
        if matches!(kind, records::StepKind::Retreat { .. }) {
            self.retreat_receipts
                .insert(event.seq, self.applications.steps.len());
        }
        self.applications.steps.push(records::ProfileStep {
            id,
            day,
            before,
            after,
            kind,
        });
        Ok(())
    }

    fn reclassified_restoration(
        &mut self,
        recorded: &RecordedDecision,
        seq: Seq,
        before: PerCore<u8>,
        after: PerCore<u8>,
    ) -> Result<records::StepKind, RunError> {
        let (retreat, event, units, reclassified_day) = recorded
            .causes
            .iter()
            .rev()
            .filter_map(|seq| self.retreat_receipts.get(seq))
            .filter_map(|&index| self.applications.steps.get(index))
            .find_map(|retreat| {
                let records::StepKind::Retreat {
                    event: Some(event),
                    units,
                } = &retreat.kind
                else {
                    return None;
                };
                self.reclassified
                    .get(event)
                    .map(|&day| (retreat.id.clone(), event.clone(), units.clone(), day))
            })
            .ok_or(RunError::Calendar(
                "reclassified restoration lacks its journaled retreat and classification",
            ))?;
        self.report.newest_step = Some(records::StepIdentity::Restoration {
            restoration: seq_id(seq),
            units: units.clone(),
        });
        let proposal = &recorded.proposal;
        Ok(records::StepKind::ReclassifiedRestoration {
            retreat,
            event,
            units,
            reclassified_day,
            hold: self
                .application_days
                .get(&proposal.hold.application)
                .filter(|_| !proposal.hold.credited.is_empty())
                .map(|&start_day| records::RestorationHold {
                    start_day,
                    profile: before,
                    clean_hours: std::time::Duration::from_secs(proposal.hold.eligible_s)
                        .as_secs_f64()
                        / 3600.0,
                    witnesses: proposal.hold.credited.iter().copied().map(seq_id).collect(),
                }),
            qualification: proposal
                .certificate
                .and_then(|seq| self.certificates.get(&seq))
                .filter(|certificate| certificate.profile == after)
                .cloned(),
            consumed_before: recorded.consumed,
            consumed_after: self
                .report
                .allowance
                .as_ref()
                .map_or(0, |ledger| ledger.consumed),
            crash_vetoes: Vec::new(),
        })
    }

    fn veto(&mut self, veto: &journal::DayVeto) -> Result<(), RunError> {
        if veto.state != journal::VetoState::Active {
            return Ok(());
        }
        let cores = match &veto.profile {
            journal::ObservedProfile::Ccd { cores, .. }
            | journal::ObservedProfile::PerCore { cores } => cores,
        };
        let profile = observed_depths(cores)?;
        let event = self.canonical_event(&veto.event);
        for step in self
            .applications
            .steps
            .iter_mut()
            .rev()
            .take_while(|step| step.after == profile)
        {
            if let records::StepKind::ReclassifiedRestoration { crash_vetoes, .. } = &mut step.kind
                && !crash_vetoes.iter().any(|veto| veto.event == event)
            {
                crash_vetoes.push(records::ProfileVeto {
                    event: event.clone(),
                    profile,
                });
            }
        }
        Ok(())
    }

    fn restoration(&mut self, event: &Event, restoration: &journal::FirmwareRestoration) {
        let day = self.day(event.time);
        match &restoration.state {
            journal::FirmwareRestorationState::Saved {} => {
                if self.restoration.is_none() && self.saved_restoration.is_none() {
                    "firmware-1".clone_into(&mut self.epoch);
                    self.stock_controls.clear();
                    self.saved_restoration = Some((event.seq, day, restoration.saved.clone()));
                }
            }
            journal::FirmwareRestorationState::SteppedBack { event: cause } => {
                let id = self.canonical_event(cause);
                if let Some(readback) = self.report.readback
                    && readback.iter().all(|(_, depth)| depth == 0)
                    && let Some(index) = self.restoration
                    && let records::RestorationOutcome::Applied { result, .. } =
                        &mut self.applications.restorations[index].outcome
                {
                    *result = records::RestorationResult::Crashed {
                        event: id,
                        stock_step_back_day: day,
                        stock_readback: readback,
                        stopped_units: self.report.stopped_units.clone(),
                        witnesses: vec![seq_id(event.seq)],
                    };
                } else {
                    self.restoration_backstep = Some(cause.clone());
                }
            }
            journal::FirmwareRestorationState::Qualified { .. }
            | journal::FirmwareRestorationState::Held { .. }
            | journal::FirmwareRestorationState::Restored { .. } => {}
        }
    }
}

fn seq_id(seq: Seq) -> String {
    format!("journal-{}", seq.get())
}
fn event_id(event: &journal::DayEventId) -> String {
    format!("{event:?}")
}
fn phase(phase: journal::UnitPhase) -> records::UnitPhase {
    match phase {
        journal::UnitPhase::Coarse => records::UnitPhase::Coarse,
        journal::UnitPhase::Core => records::UnitPhase::Core,
    }
}
fn classification(value: journal::Classification) -> records::Classification {
    match value {
        journal::Classification::CpuSuspect { .. } => records::Classification::CpuSuspect,
        journal::Classification::NonCpu {} => records::Classification::NonCpu,
        journal::Classification::Unexplained {} => records::Classification::Unexplained,
    }
}
fn unit_id(unit: journal::UnitId) -> Result<records::UnitId, RunError> {
    let index = match unit {
        journal::UnitId::Ccd(ccd) => ccd.index(),
        journal::UnitId::Core(core) => core.index(),
    };
    records::UnitId::new(
        u16::try_from(index).map_err(|_| RunError::Calendar("successor unit index"))?,
    )
    .map_err(|_| RunError::Calendar("successor unit index"))
}
fn membership(mapping: &journal::UnitMapping) -> Result<Vec<records::PolicyUnit>, RunError> {
    mapping
        .units
        .iter()
        .map(|unit| {
            Ok(records::PolicyUnit {
                unit: unit_id(unit.unit)?,
                cores: unit.cores.iter().collect(),
                kind: phase(mapping.phase),
                parent: unit.parent.map(unit_id).transpose()?,
            })
        })
        .collect()
}
fn depths(profile: &PerCore<TargetOffset>) -> Result<PerCore<u8>, RunError> {
    let mut result = PerCore::default();
    for (core, value) in profile.iter() {
        result.set(core, super::depth(value)?);
    }
    Ok(result)
}
fn observed_depths(
    profile: &PerCore<togi_machine::ObservedOffset>,
) -> Result<PerCore<u8>, RunError> {
    let mut result = PerCore::default();
    for (core, value) in profile.iter() {
        result.set(
            core,
            u8::try_from(-value.0).map_err(|_| RunError::Calendar("journal readback depth"))?,
        );
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::day_night::{CalendarConfig, DayStart, EndAnswer, run_controlled};

    struct Interrupted {
        policy: SuccessorPolicy,
        interrupted: bool,
    }

    impl Policy for Interrupted {
        fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError> {
            self.policy.firmware_changed(machine)
        }
        fn start_day(&mut self, day: u32, machine: &mut Machine<'_>) -> Result<(), RunError> {
            self.policy.start_day(day, machine)
        }
        fn observe_day(
            &mut self,
            day: u32,
            evidence: &DayObservation,
            machine: &mut Machine<'_>,
        ) -> Result<DayClassification, RunError> {
            if !self.interrupted
                && self
                    .policy
                    .applications()
                    .steps
                    .iter()
                    .any(|step| matches!(step.kind, records::StepKind::Climb { .. }))
            {
                self.interrupted = true;
                let active = match evidence {
                    DayObservation::Completed { active } | DayObservation::Ended { active, .. } => {
                        *active
                    }
                };
                return self.policy.observe_day(
                    day,
                    &DayObservation::Ended {
                        active,
                        answer: Some(EndAnswer::Cpu),
                    },
                    machine,
                );
            }
            self.policy.observe_day(day, evidence, machine)
        }
        fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError> {
            self.policy.night(night, machine)
        }
    }

    #[test]
    fn seeded_report_and_applications_are_exactly_the_journal_fold() {
        let mut run = Interrupted {
            policy: SuccessorPolicy::new(1.5).unwrap().recording(),
            interrupted: false,
        };
        run_controlled(
            "edge-deep",
            20001,
            CalendarConfig {
                days: 120,
                ..CalendarConfig::default()
            },
            &mut run,
            |_, policy| {
                if policy.policy.report().stopped_units.is_empty() {
                    DayStart::Run
                } else {
                    DayStart::Stop
                }
            },
        )
        .unwrap();
        assert!(run.interrupted);
        let report = run.policy.report();
        let applications = run.policy.applications();
        let events = run.policy.events().unwrap();
        let mut replay = JournalFold::new();
        for event in events {
            replay.fold(event).unwrap();
        }
        assert_eq!(report, replay.report);
        assert_eq!(applications, replay.applications);
        assert!(!applications.steps.is_empty());
        assert!(!report.finishes.is_empty());
        assert!(
            report
                .finishes
                .iter()
                .any(|finish| finish.reason == records::FinishReason::CpuSuspectStop)
        );
        let (stop_event, stop) = events
            .iter()
            .find_map(|event| {
                if let Payload::UnitStop(stop) = &event.payload
                    && matches!(stop.reason, journal::UnitStopReason::FirstBackstep { .. })
                {
                    Some((event, stop))
                } else {
                    None
                }
            })
            .expect("journaled first backstep");
        let finish = report
            .finishes
            .iter()
            .find(|finish| finish.unit == unit_id(stop.unit).unwrap())
            .expect("finish for the stopped unit");
        assert_eq!(finish.reason, records::FinishReason::CpuSuspectStop);
        assert_eq!(
            finish.day,
            1.0 + stop_event.time.duration_since(events[0].time).as_secs_f64() / 86_400.0
        );
        let stops = events
            .iter()
            .filter_map(|event| {
                if let Payload::UnitStop(stop) = &event.payload {
                    Some(unit_id(stop.unit).unwrap())
                } else {
                    None
                }
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            report
                .stopped_units
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            stops
        );
        let consumed = events
            .iter()
            .filter_map(|event| {
                if let Payload::CampaignCharge(charge) = &event.payload {
                    Some(u64::from(charge.consumed))
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0);
        assert_eq!(report.allowance.as_ref().unwrap().consumed, consumed);
        assert_eq!(consumed, 1);
        assert_eq!(report.allowance.as_ref().unwrap().limit, 2);
        assert_eq!(
            u64::try_from(report.events.iter().filter(|event| event.charged).count()).unwrap(),
            consumed
        );
        for event in events {
            if let Payload::TrialEnd(end) = &event.payload
                && end.outcome == journal::Outcome::Failure
            {
                let id = format!("journal-failure-{}", end.trial.as_str());
                assert_eq!(
                    report
                        .events
                        .iter()
                        .filter(|reported| reported.id == id)
                        .count(),
                    1
                );
            }
        }
        for step in &applications.steps {
            let applied = events
                .iter()
                .find_map(|event| {
                    if let Payload::DayApplied(applied) = &event.payload
                        && let journal::ApplyCause::Step { decision } = applied.cause
                        && step.id == format!("journal-{}", decision.0)
                    {
                        Some(applied)
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(step.after, observed_depths(&applied.readbacks).unwrap());
            if let records::StepKind::Climb { qualification, .. } = &step.kind {
                assert!(!qualification.fresh_witnesses.is_empty());
                for id in qualification
                    .fresh_witnesses
                    .iter()
                    .chain(&qualification.reused_witnesses)
                    .chain(&qualification.stock_controls)
                {
                    assert!(events.iter().any(|event| {
                        matches!(&event.payload, Payload::ExposureObserved(exposure)
                            if exposure.source.trial.as_ref().is_some_and(|trial| trial.as_str() == id))
                    }));
                }
            }
        }
        assert_reclassified_restoration(events, replay);
    }

    fn assert_reclassified_restoration(events: &[Event], mut fold: JournalFold) {
        // Extend the real run with an explicit journal fixture: an operator correction and a
        // causally linked restoration. Missing hold evidence must remain absent, not fabricated.
        let (retreat_receipt, retreat) = events
            .iter()
            .rev()
            .find_map(|event| {
                let Payload::DayApplied(applied) = &event.payload else {
                    return None;
                };
                let journal::ApplyCause::Step { decision } = applied.cause else {
                    return None;
                };
                events.iter().find_map(|proposal| {
                    let Payload::ClimbDecision(step) = &proposal.payload else {
                        return None;
                    };
                    (proposal.seq.get() == decision.0
                        && matches!(step.decision, journal::ClimbAction::Retreat { .. }))
                    .then_some((event, step))
                })
            })
            .expect("applied retreat");
        let promotion = events
            .iter()
            .find_map(|event| {
                let Payload::ClimbDecision(step) = &event.payload else {
                    return None;
                };
                matches!(step.decision, journal::ClimbAction::Promote { .. })
                    .then_some((event.seq, step))
            })
            .expect("applied promotion proposal");
        let mut receipt = events.iter().find_map(|event| {
            let Payload::DayApplied(applied) = &event.payload else { return None; };
            matches!(applied.cause, journal::ApplyCause::Step { decision } if decision.0 == promotion.0.get())
                .then_some(applied.clone())
        }).expect("promotion receipt");
        let charge = events
            .iter()
            .find_map(|event| {
                if let Payload::CampaignCharge(charge) = &event.payload {
                    Some(charge)
                } else {
                    None
                }
            })
            .expect("charge");
        let original = events
            .iter()
            .find_map(|event| {
                if let Payload::DayEvent(observed) = &event.payload
                    && observed.event == charge.event
                {
                    Some(observed.classification)
                } else {
                    None
                }
            })
            .expect("classified event");
        let mut event = events.last().unwrap().clone();
        event.seq = Seq::new(event.seq.get() + 1).unwrap();
        event.payload = Payload::CampaignReclassified(journal::CampaignReclassified {
            campaign: charge.campaign,
            event: charge.event.clone(),
            from: original,
            to: journal::Classification::NonCpu {},
        });
        fold.fold(&event).unwrap();
        let mut proposal = promotion.1.clone();
        proposal.before = retreat.proposed.clone();
        proposal.proposed = retreat.before.clone();
        proposal.decision = journal::ClimbAction::Restore {
            restoration: journal::RestorationKind::Reclassified,
            units: proposal
                .proposed
                .mapping
                .units
                .iter()
                .map(|unit| unit.unit)
                .collect(),
        };
        proposal.hold.credited.clear();
        event.seq = Seq::new(event.seq.get() + 1).unwrap();
        let decision = event.seq;
        event.cause = vec![retreat_receipt.seq];
        event.payload = Payload::ClimbDecision(proposal);
        fold.fold(&event).unwrap();
        event.seq = Seq::new(event.seq.get() + 1).unwrap();
        receipt.application = journal::ApplicationId(event.seq.get());
        receipt.cause = journal::ApplyCause::Step {
            decision: journal::DecisionId(decision.get()),
        };
        event.payload = Payload::DayApplied(receipt);
        fold.fold(&event).unwrap();
        let restored = fold.applications.steps.last().unwrap();
        let records::StepKind::ReclassifiedRestoration {
            retreat: linked,
            units,
            hold,
            qualification,
            consumed_before,
            consumed_after,
            ..
        } = &restored.kind
        else {
            panic!("distinct reclassified restoration");
        };
        let original = fold
            .applications
            .steps
            .iter()
            .find(|step| &step.id == linked)
            .unwrap();
        let records::StepKind::Retreat {
            units: reverted, ..
        } = &original.kind
        else {
            panic!("retreat link");
        };
        assert_eq!(units, reverted);
        assert!(hold.is_none());
        assert!(qualification.is_some());
        assert_eq!((*consumed_before, *consumed_after), (1, 1));
        assert_eq!(fold.report.allowance.unwrap().consumed, 1);
    }
}
