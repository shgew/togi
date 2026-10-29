//! Read-only successor diagnostics from the adapter's already-folded journal. Nothing here feeds a
//! policy query or appends an event; recording the full journal remains optional.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use togi_journal::{
    Activity, ApplyCause, CcdVector, CellEvidence, DayApplied, DayEventId, DayStep, DecisionId,
    Event, Outcome, Payload, QualificationState, RetreatCause, Seq, SessionContext, StepKind,
};
use togi_machine::{CcdId, PerCore, TargetOffset};

use super::super::successor::{
    Classification, FirmwareRestoration, LearningEvent, Phase, PolicyApplications, PolicyUnit,
    ProfileStep, Qualification, RestorationOutcome, RestorationResult, StepKind as ReportStep,
    UnitId, UnitPhase,
};

#[derive(Default)]
pub(super) struct Witnesses {
    applications: PolicyApplications,
    night_events: Vec<LearningEvent>,
    units: BTreeMap<CcdId, UnitId>,
    membership: Vec<PolicyUnit>,
    context: Option<SessionContext>,
    firmware_changes: u64,
    transition_day: Option<f64>,
    steps: BTreeMap<DecisionId, DayStep>,
    caps: BTreeMap<Seq, (CcdId, u8)>,
    exposures: BTreeMap<Seq, String>,
    failures: BTreeMap<Seq, String>,
    requalifications: HashMap<DayEventId, String>,
    controls: Vec<String>,
    qualifications: BTreeMap<Seq, Qualification>,
}

impl Witnesses {
    pub(super) fn applications(&self) -> PolicyApplications {
        self.applications.clone()
    }

    pub(super) fn night_events(&self) -> &[LearningEvent] {
        &self.night_events
    }

    fn epoch(&self) -> String {
        if self.firmware_changes == 0 {
            "initial".to_owned()
        } else {
            format!("firmware-{}", self.firmware_changes)
        }
    }

    pub(super) fn observe(&mut self, event: &Event) {
        let Some(mono) = event.mono else {
            return;
        };
        let day = 1.0 + mono.as_secs_f64() / 86_400.0;
        if let Payload::SessionStart(start) = &event.payload {
            let ccds: std::collections::BTreeSet<_> =
                start.cores.iter().map(|info| info.ccd).collect();
            for (index, ccd) in (0_u16..).zip(ccds) {
                if let Ok(unit) = UnitId::new(index) {
                    self.units.insert(ccd, unit);
                    self.membership.push(PolicyUnit {
                        unit,
                        cores: start
                            .cores
                            .iter()
                            .filter(|info| info.ccd == ccd)
                            .map(|info| info.core)
                            .collect(),
                        kind: UnitPhase::Coarse,
                        parent: None,
                    });
                }
            }
        }
        if let Payload::SessionContext(context) = &event.payload {
            if self.context.as_ref().is_some_and(|old| old != context) {
                self.firmware_changes += 1;
                self.transition_day = Some(day);
                self.controls.clear();
            }
            self.context = Some(context.clone());
        }
        if let Payload::DayStep(step) = &event.payload {
            self.steps.insert(step.decision, step.clone());
        }
        if let Payload::CcdCap(cap) = &event.payload
            && let Some(depth) = depth(cap.new)
        {
            self.caps.insert(event.seq, (cap.ccd, depth));
        }
        if let Payload::ExposureObserved(exposure) = &event.payload
            && let Some(trial) = &exposure.source.trial
        {
            let id = trial.as_str().to_owned();
            self.exposures.insert(event.seq, id.clone());
            if exposure.outcome == Outcome::Failure {
                self.failures
                    .insert(event.seq, format!("ruleset13-failure-{}", trial.as_str()));
            }
            if exposure.activity == Activity::Control && exposure.outcome == Outcome::Pass {
                self.controls.push(id);
            }
        }
        if let Payload::NightQualification(qualification) = &event.payload
            && let QualificationState::Complete { witnesses } = &qualification.state
            && let Some(profile) = profile(&qualification.profile)
        {
            let mut fresh = Vec::new();
            let mut reused = Vec::new();
            for witness in witnesses {
                let (exposure, destination) = match witness.evidence {
                    CellEvidence::Fresh { exposure } => (exposure, &mut fresh),
                    CellEvidence::Reused { exposure } => (exposure, &mut reused),
                };
                if let Some(trial) = self.exposures.get(&exposure)
                    && !destination.contains(trial)
                {
                    destination.push(trial.clone());
                }
            }
            self.qualifications.insert(
                event.seq,
                Qualification {
                    id: format!("ruleset13-qualification-{}", event.seq),
                    day,
                    epoch: self.epoch(),
                    profile,
                    stock_controls: self
                        .controls
                        .iter()
                        .filter(|id| !fresh.contains(id) && !reused.contains(id))
                        .cloned()
                        .collect(),
                    fresh_witnesses: fresh,
                    reused_witnesses: reused,
                    // Only emitted on application of a promotion citing this completed certificate.
                    safety_checks_satisfied: true,
                },
            );
        }
        if let Payload::NightQualification(qualification) = &event.payload
            && let QualificationState::Failed { failure, .. } = &qualification.state
            && let Some(id) = self.failures.get(failure)
        {
            self.failures.insert(event.seq, id.clone());
        }
        if let Payload::DayEvent(daily) = &event.payload
            && matches!(daily.event, DayEventId::Requalification { .. })
            && let Some(id) = daily
                .witnesses
                .iter()
                .find_map(|seq| self.failures.get(seq))
        {
            // Re-qualification is both a failed night trial and a daily escalation source.
            // Its retreat names that same failure, not a second synthetic interruption.
            self.requalifications
                .insert(daily.event.clone(), id.clone());
        }
        if let Payload::DayApplied(applied) = &event.payload {
            self.applied(applied, day);
        }
        if let Payload::TrialEnd(end) = &event.payload
            && end.outcome == Outcome::Failure
        {
            // The session adjudicator has already decided this is a conclusive failure.
            // Interruptions without failure evidence and inconclusive backend/placement outcomes
            // are not classified here. Location stays in A's journal, never inferred from truth.
            self.night_events.push(LearningEvent {
                id: format!("ruleset13-failure-{}", end.trial.as_str()),
                day,
                classification: Classification::CpuSuspect,
                provisional: false,
                reclassified_from: None,
                charged: false,
                phase: Phase::Learning,
                unit_phase: UnitPhase::Coarse,
                newest_step: None,
            });
        }
    }

    fn applied(&mut self, applied: &DayApplied, day: f64) {
        let ApplyCause::Step { decision } = applied.cause else {
            return;
        };
        let Some(step) = self.steps.remove(&decision) else {
            return;
        };
        // Only the verified application commits a decision. Holds and unchanged reapplications
        // are not steps, and an uncommitted proposal never appears in the report.
        if applied.target != step.proposed {
            return;
        }
        let (Some(before), Some(after)) = (profile(&step.old), profile(&applied.target)) else {
            return;
        };
        let kind = match &step.step {
            StepKind::Hold { .. } => return,
            StepKind::Promote { changed } => {
                let Some(&unit) = self.units.get(changed) else {
                    return;
                };
                let Some(qualification) = step
                    .causes
                    .qualification
                    .and_then(|seq| self.qualifications.get(&seq))
                    .filter(|qualification| qualification.profile == after)
                    .cloned()
                else {
                    return;
                };
                let Some(ceiling_depth) = step.causes.caps.iter().find_map(|seq| {
                    self.caps
                        .get(seq)
                        .filter(|(ccd, _)| ccd == changed)
                        .map(|(_, depth)| *depth)
                }) else {
                    return;
                };
                if let Some(restoration) = self.applications.restorations.last_mut()
                    && restoration.previous_profile == after
                    && before.iter().all(|(_, depth)| depth == 0)
                    && matches!(restoration.outcome, RestorationOutcome::Blocked { .. })
                {
                    restoration.outcome = RestorationOutcome::Applied {
                        day,
                        qualification,
                        clean_stock_hours: Duration::from_secs(step.hold_s).as_secs_f64() / 3600.0,
                        result: RestorationResult::Retained {},
                    };
                    ReportStep::Restoration {
                        restoration: restoration.id.clone(),
                    }
                } else {
                    ReportStep::Climb {
                        unit,
                        ceiling_depth,
                        clean_hold_hours: Duration::from_secs(step.hold_s).as_secs_f64() / 3600.0,
                        qualification,
                    }
                }
            }
            StepKind::Retreat { changed, cause, .. } => {
                if changed.is_empty() {
                    return;
                }
                let event = match cause {
                    RetreatCause::Event { event, .. } => Some(
                        self.requalifications
                            .get(event)
                            .cloned()
                            .unwrap_or_else(|| event_id(event)),
                    ),
                    RetreatCause::ShallowerCap {} | RetreatCause::Completion {} => None,
                };
                if let Some(restoration) = self.applications.restorations.last_mut()
                    && restoration.previous_profile == before
                    && after.iter().all(|(_, depth)| depth == 0)
                    && let Some(event) = &event
                    && let RestorationOutcome::Applied { result, .. } = &mut restoration.outcome
                {
                    *result = RestorationResult::Crashed {
                        event: event.clone(),
                        stock_step_back_day: day,
                        stock_readback: after,
                        // A retreat is not a successor unit-stop commitment.
                        stopped_units: Vec::new(),
                        witnesses: vec![format!("ruleset13-step-{}", decision.0)],
                    };
                }
                ReportStep::Retreat {
                    event,
                    units: changed
                        .iter()
                        .filter_map(|ccd| self.units.get(ccd).copied())
                        .collect(),
                }
            }
            StepKind::FirmwareZero { .. } => {
                let epoch = self.epoch();
                if let Some(transition_day) = self.transition_day {
                    self.applications.restorations.push(FirmwareRestoration {
                        id: format!("ruleset13-restoration-{epoch}"),
                        epoch: epoch.clone(),
                        transition_day,
                        previous_profile: before,
                        membership: self.membership.clone(),
                        stock_reset_day: day,
                        stock_readback: after,
                        outcome: RestorationOutcome::Blocked {
                            reason: "no qualified whole-profile restoration has been applied"
                                .to_owned(),
                        },
                    });
                }
                ReportStep::StockReset { epoch }
            }
        };
        self.applications.steps.push(ProfileStep {
            id: format!("ruleset13-step-{}", decision.0),
            day,
            before,
            after,
            kind,
        });
    }
}

pub(super) fn event_id(event: &DayEventId) -> String {
    match event {
        DayEventId::Boot { boot } => format!("ruleset13-event-boot-{boot}"),
        DayEventId::Record { session, seq } => format!("ruleset13-event-record-{session}-{seq}"),
        DayEventId::Requalification { night } => format!("ruleset13-event-requalification-{night}"),
    }
}

fn depth(offset: TargetOffset) -> Option<u8> {
    u8::try_from(-offset.get()).ok()
}

fn profile(vector: &CcdVector) -> Option<PerCore<u8>> {
    vector
        .cores()
        .iter()
        .map(|(core, offset)| depth(offset).map(|depth| (core, depth)))
        .collect()
}
