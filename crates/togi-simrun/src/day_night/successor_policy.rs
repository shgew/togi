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

/// Bounded by decisions, certificates and reported events, not by trial samples.
pub(super) struct JournalFold {
    pub(super) report: PolicyReport,
    pub(super) applications: PolicyApplications,
    origin: Option<Timestamp>,
    decisions: BTreeMap<Seq, journal::ClimbDecision>,
    certificates: BTreeMap<Seq, records::Qualification>,
    ceilings: BTreeMap<journal::UnitId, u8>,
    mapping: Option<journal::UnitMapping>,
    epoch: String,
    stock_controls: Vec<(togi_machine::BiosContext, u32, String)>,
    restoration: Option<usize>,
    saved_restoration: Option<(Seq, f64, journal::UnitVector)>,
    restoration_backstep: Option<journal::DayEventId>,
}

impl JournalFold {
    pub(super) fn new() -> Self {
        Self {
            report: PolicyReport::unavailable(),
            applications: PolicyApplications::default(),
            origin: None,
            decisions: BTreeMap::new(),
            certificates: BTreeMap::new(),
            ceilings: BTreeMap::new(),
            mapping: None,
            epoch: "initial".to_owned(),
            stock_controls: Vec::new(),
            restoration: None,
            saved_restoration: None,
            restoration_backstep: None,
        }
    }

    fn day(&self, time: Timestamp) -> f64 {
        self.origin.map_or(1.0, |origin| {
            1.0 + time.duration_since(origin).as_secs_f64() / 86_400.0
        })
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
                    self.decisions.insert(event.seq, decision.clone());
                }
            }
            Payload::ExposureObserved(exposure)
                if exposure.activity == journal::Activity::Control
                    && exposure.outcome == journal::Outcome::Pass
                    && exposure
                        .profile
                        .cores()
                        .iter()
                        .all(|(_, offset)| offset == TargetOffset::ZERO)
                    && exposure.exclusions.is_empty()
                    && exposure.failures.is_empty() =>
            {
                self.stock_controls.push((
                    exposure.firmware.clone(),
                    exposure.epoch,
                    seq_id(event.seq),
                ));
            }
            Payload::NightQualification(certificate) => {
                if let journal::QualificationState::Complete { witnesses } = &certificate.state {
                    let mut fresh = Vec::new();
                    let mut reused = Vec::new();
                    for witness in witnesses {
                        match witness.evidence {
                            journal::CellEvidence::Fresh { exposure } => {
                                fresh.push(seq_id(exposure));
                            }
                            journal::CellEvidence::Reused { exposure } => {
                                reused.push(seq_id(exposure));
                            }
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
                    witnesses.push(event_id(triggering));
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
                let id = event_id(&observed.event);
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
                let id = event_id(&charge.event);
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
                let id = event_id(&reclassified.event);
                if let Some(item) = self.report.events.iter_mut().find(|item| item.id == id) {
                    item.reclassified_from = Some(classification(reclassified.from));
                    item.classification = classification(reclassified.to);
                    item.provisional = false;
                }
                self.update_counts();
            }
            Payload::FirmwareRestoration(restoration) => self.restoration(event, restoration),
            Payload::UnitMapping(mapping) => self.mapping(mapping)?,
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
            | Payload::TrialEnd(_)
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
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::TaskStart(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayVeto(_)
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
            if let Some(cause) = self.restoration_backstep.take()
                && let Some(index) = self.restoration
                && let records::RestorationOutcome::Applied { result, .. } =
                    &mut self.applications.restorations[index].outcome
            {
                *result = records::RestorationResult::Crashed {
                    event: event_id(&cause),
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
        let proposal = self.decisions.get(&seq).cloned().ok_or(RunError::Calendar(
            "application has no journaled climb decision",
        ))?;
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
                    qualification: self.certificate(&proposal)?,
                }
            }
            journal::ClimbAction::Retreat { units, cause, .. } => records::StepKind::Retreat {
                event: match cause {
                    journal::ClimbRetreatCause::Event { event }
                    | journal::ClimbRetreatCause::Veto { event } => Some(event_id(event)),
                    journal::ClimbRetreatCause::AdverseCeiling { .. }
                    | journal::ClimbRetreatCause::Completion {} => None,
                },
                units: units
                    .iter()
                    .copied()
                    .map(unit_id)
                    .collect::<Result<_, _>>()?,
            },
            journal::ClimbAction::FirmwareZero {} => records::StepKind::StockReset {
                epoch: self.epoch.clone(),
            },
            journal::ClimbAction::Restore { restoration, units } => {
                let restoration_id = match restoration {
                    journal::RestorationKind::Firmware => {
                        let index = self.restoration.ok_or(RunError::Calendar(
                            "firmware application has no saved journal profile",
                        ))?;
                        let qualification = self.certificate(&proposal)?;
                        let item = &mut self.applications.restorations[index];
                        item.outcome = records::RestorationOutcome::Applied {
                            day,
                            qualification,
                            clean_stock_hours: std::time::Duration::from_secs(
                                proposal.hold.eligible_s,
                            )
                            .as_secs_f64()
                                / 3600.0,
                            result: records::RestorationResult::Retained {},
                        };
                        item.id.clone()
                    }
                    journal::RestorationKind::Reclassified => return Ok(()),
                };
                self.report.newest_step = Some(records::StepIdentity::Restoration {
                    restoration: restoration_id.clone(),
                    units: units
                        .iter()
                        .copied()
                        .map(unit_id)
                        .collect::<Result<_, _>>()?,
                });
                records::StepKind::Restoration {
                    restoration: restoration_id,
                }
            }
        };
        self.applications.steps.push(records::ProfileStep {
            id,
            day,
            before,
            after,
            kind,
        });
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
                if let Some(readback) = self.report.readback
                    && readback.iter().all(|(_, depth)| depth == 0)
                    && let Some(index) = self.restoration
                    && let records::RestorationOutcome::Applied { result, .. } =
                        &mut self.applications.restorations[index].outcome
                {
                    *result = records::RestorationResult::Crashed {
                        event: event_id(cause),
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
        }
    }
}
