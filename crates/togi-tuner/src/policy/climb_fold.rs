//! Decode additive D commitments without changing the released CCD history's meaning.

use togi_journal::{self as journal, Event, Seq};
use crate::{UnitProfile, Units, climb, day::{self, ApplicationKind, DailyEvent}};
use super::{State, FoldError, books::Committed, permission::{ceiling, mapping}};

fn committed<T>(event: &Event, payload: T) -> Committed<T> {
    Committed { seq: event.seq, time: event.time, causes: event.cause.clone(), payload }
}

impl State {
    pub(super) fn units_for(&self, recorded: &journal::UnitMapping) -> Result<Units, FoldError> {
        let topology = self.machine.topology.as_ref().ok_or(FoldError::ClimbRecord("missing topology"))?;
        let units = match recorded.phase { journal::UnitPhase::Coarse => Units::coarse(topology), journal::UnitPhase::Core => Units::cores(topology) };
        if mapping(&units) != *recorded { return Err(FoldError::ClimbRecord("unit mapping differs from topology")); }
        Ok(units)
    }

    pub(super) fn unit_profile(&self, vector: &journal::UnitVector) -> Result<UnitProfile, FoldError> {
        let units = self.units_for(&vector.mapping)?;
        let mut values = Vec::new();
        for unit in units.iter() {
            let mut offsets = unit.cores.iter().map(|core| vector.cores.get(core));
            let offset = offsets.next().flatten().ok_or(FoldError::ClimbRecord("incomplete vector"))?;
            if offsets.any(|other| other != Some(offset)) { return Err(FoldError::ClimbRecord("nonuniform tuning unit")); }
            values.push((unit.id, offset));
        }
        let profile = UnitProfile::try_from(values).map_err(climb::ClimbError::from)?;
        if profile.expand(&units).map_err(climb::ClimbError::from)? != vector.cores { return Err(FoldError::ClimbRecord("extra physical core")); }
        Ok(profile)
    }

    pub(super) fn fold_mapping(&mut self, event: &Event, recorded: &journal::UnitMapping) -> Result<(), FoldError> {
        let units = self.units_for(recorded)?;
        if let Some(history) = &mut self.climb.history
            && history.units() != &units
        {
            let cores = history.current().expand(history.units()).map_err(climb::ClimbError::from)?;
            history.refine(units, UnitProfile::of_cores(&cores))?;
        }
        self.climb.mapping = Some(committed(event, recorded.clone()));
        Ok(())
    }

    pub(super) fn fold_climb_decision(&mut self, event: &Event, recorded: &journal::ClimbDecision) -> Result<(), FoldError> {
        let before = self.unit_profile(&recorded.before)?;
        let target = self.unit_profile(&recorded.proposed)?;
        let history = self.climb.history.as_ref().ok_or(FoldError::ClimbRecord("missing baseline"))?;
        if history.current() != &before { return Err(FoldError::ClimbRecord("decision differs from applied profile")); }
        let ceilings = self.ceilings();
        let view = self.day_view().map_err(|_| FoldError::ClimbRecord("invalid daily history"))?;
        let vetoes = day::vetoes(&view.events);
        let action = match &recorded.decision {
            journal::ClimbAction::Hold { .. } => climb::ClimbAction::Hold { blocked_by: Vec::new() },
            journal::ClimbAction::Promote { unit, from, to, step } => {
                if before.get(*unit) != Some(*from) || target != before.with(*unit, *to) { return Err(FoldError::ClimbRecord("promotion vector differs")); }
                climb::ClimbAction::Promote(climb::Proposal {
                    unit: *unit, from: *from, to: *to, remaining: ceilings.get(unit).and_then(|ceiling| ceiling.c).map_or(0, |c| from.get() - c.get()),
                    step: i64::from(*step), profile: target,
                })
            }
            journal::ClimbAction::FirmwareZero {} => climb::ClimbAction::FirmwareZero {
                saved: history.saved().cloned().unwrap_or_else(|| climb::SavedProfile { units: history.units().clone(), profile: before.clone() }), profile: target,
            },
            journal::ClimbAction::Restore { restoration, .. } => {
                let proposal = climb::restoration_candidate(history, &view.events)?.ok_or(FoldError::ClimbRecord("missing restoration ancestry"))?;
                let kind = match restoration {
                    journal::RestorationKind::Firmware => climb::RestorationKind::Firmware,
                    journal::RestorationKind::Reclassified => proposal.kind,
                };
                if proposal.profile != target { return Err(FoldError::ClimbRecord("restoration vector differs")); }
                climb::ClimbAction::Restore(climb::RestorationProposal { kind, profile: target })
            }
            journal::ClimbAction::Retreat { cause, wanted, .. } => {
                let pending = climb::retreat_plan(history, &view.events, &vetoes, &ceilings)?;
                let mut retreat = pending.unwrap_or_else(|| climb::Retreat {
                    profile: target.clone(), wanted: target.clone(), events: Vec::new(), cursors: Vec::new(), stopped: Vec::new(), adverse_units: Vec::new(),
                });
                retreat.profile = target.clone();
                retreat.wanted = wanted.as_ref().map(|vector| self.unit_profile(vector)).transpose()?.unwrap_or_else(|| target.clone());
                if matches!(cause, journal::ClimbRetreatCause::Completion {}) {
                    retreat.events = history.retreats().last().map_or_else(Vec::new, |record| record.events.clone());
                }
                for (unit, depth) in target.iter() {
                    if before.get(unit).is_some_and(|old| old < depth) && !retreat.stopped.contains(&unit) { retreat.stopped.push(unit); }
                }
                climb::ClimbAction::Retreat(retreat)
            }
        };
        self.climb.plans.insert(event.seq, climb::ClimbDecision { before, ceilings, action });
        self.climb.decisions.insert(event.seq, committed(event, recorded.clone()));
        Ok(())
    }

    pub(super) fn fold_campaign_start(&mut self, event: &Event, start: &journal::CampaignStart) {
        // An existing campaign is never replaced by another session's fresh allowance.
        if self.climb.start.is_some() { return; }
        if start.allowance != 2 || !matches!(start.history, journal::CampaignHistory::Complete {}) {
            self.climb.campaign = climb::Campaign::Unreconstructible;
        }
        self.climb.start = Some(committed(event, start.clone()));
        let mut events = self.days.event_commits.iter().map(|(&seq, payload)| (seq, payload.clone())).collect::<Vec<_>>();
        events.sort_by_key(|(seq, _)| *seq);
        for (_, payload) in events { self.observe_campaign_event(&payload); }
        self.observe_finishes(event.time);
    }

    /// Capture classifications at raw prefixes, including provisional events whose derived
    /// receipt is interrupted or whose answer arrives before the next policy query.
    pub(super) fn observe_raw_campaign(&mut self, at: jiff::Timestamp) -> Result<(), FoldError> {
        let Some(history) = self.climb.history.as_ref() else { return Ok(()); };
        let phase = if history.current().units().all(|unit| history.finishes().is_finished(unit)) {
            climb::EventPhase::PostFinish
        } else { climb::EventPhase::Learning };
        let unit_phase = history.units().phase();
        let view = self.day_view().map_err(|_| FoldError::ClimbRecord("invalid raw campaign history"))?;
        for event in &view.events {
            self.climb.campaign.observe(event, &[], phase, unit_phase);
        }
        self.observe_finishes(at);
        Ok(())
    }

    pub(super) fn observe_campaign_event(&mut self, recorded: &journal::DayEvent) {
        let Some(id) = self.policy_event_id(&recorded.event) else { return; };
        let profile = match &recorded.profile {
            journal::ObservedProfile::Ccd { cores, .. } | journal::ObservedProfile::PerCore { cores } => day::CoreProfile::new(*cores),
        };
        let Ok(profile) = profile else { return; };
        let class = match recorded.classification {
            journal::Classification::CpuSuspect { certainty: journal::Certainty::Definite } => day::Classification::CpuSuspect(day::CpuCause::CpuFaultEnd),
            journal::Classification::CpuSuspect { certainty: journal::Certainty::Provisional } => day::Classification::ProvisionalCpuSuspect(day::ProvisionalCause::UnansweredUnknown),
            journal::Classification::NonCpu {} => day::Classification::NonCpu,
            journal::Classification::Unexplained {} => day::Classification::PendingQuestion,
        };
        let Some(history) = &self.climb.history else { return; };
        let phase = if history.current().units().all(|unit| history.finishes().is_finished(unit)) { climb::EventPhase::PostFinish } else { climb::EventPhase::Learning };
        let event = DailyEvent { id, profile, witnesses: Vec::new(), class, question: day::QuestionState::NotAsked };
        self.climb.campaign.observe(&event, &[], phase, history.units().phase());
    }

    pub(super) fn observe_finishes(&mut self, at: jiff::Timestamp) {
        let ceilings = self.ceilings();
        if let Some(history) = &mut self.climb.history {
            let current = history.current().clone();
            history.finishes_mut().observe(at, &current, &ceilings, self.climb.campaign.exhausted());
        }
    }

    pub(super) fn fold_unit_stop(&mut self, event: &Event, stop: &journal::UnitStop) -> Result<(), FoldError> {
        let profile = self.unit_profile(&stop.profile)?;
        let reason = match &stop.reason {
            journal::UnitStopReason::Ceiling {} => climb::FinishReason::Ceiling,
            journal::UnitStopReason::AllowanceExhausted {} => climb::FinishReason::AllowanceExhausted,
            journal::UnitStopReason::AdverseCeiling {} => climb::FinishReason::Stopped(climb::StopReason::AdverseCeiling),
            journal::UnitStopReason::Veto {} => climb::FinishReason::Stopped(climb::StopReason::Veto),
            journal::UnitStopReason::Predecessor { original } => climb::FinishReason::Stopped(climb::StopReason::Predecessor { original: *original }),
            journal::UnitStopReason::FirstBackstep { event, application } => climb::FinishReason::Stopped(climb::StopReason::FirstBackstep {
                event: self.policy_event_id(event).ok_or(FoldError::ClimbRecord("missing event identity"))?,
                application: climb::ApplicationId::new(application.0),
            }),
            journal::UnitStopReason::RestorationBackstep { event: id } => climb::FinishReason::Stopped(climb::StopReason::RestorationBackstep {
                event: self.policy_event_id(id).ok_or(FoldError::ClimbRecord("missing event identity"))?,
                application: climb::ApplicationId::new(event.seq.get()).ok_or(FoldError::ClimbRecord("invalid application identity"))?,
            }),
        };
        if let Some(history) = &mut self.climb.history
            && history.finishes().get(stop.unit).is_none()
        {
            history.finishes_mut().stop(stop.unit, climb::FinishWitness {
                reason, at: event.time, profile, last_clean: stop.last_clean,
                ceiling: self.nights.unit_edges.get(&stop.unit).map(|edge| ceiling(&edge.payload.new)),
            });
        }
        self.climb.stops.insert(stop.unit, committed(event, stop.clone()));
        Ok(())
    }

    pub(super) fn climb_application_kind(&self, applied: &journal::DayApplied) -> Option<ApplicationKind> {
        let journal::ApplyCause::Step { decision } = applied.cause else { return None; };
        let seq = Seq::new(decision.0)?;
        let plan = self.climb.plans.get(&seq)?;
        Some(match &plan.action {
            climb::ClimbAction::Promote(_) | climb::ClimbAction::Restore(_) => ApplicationKind::Promotion,
            climb::ClimbAction::FirmwareZero { .. } => ApplicationKind::FirmwareZero,
            climb::ClimbAction::Hold { .. } => ApplicationKind::Reapplication,
            climb::ClimbAction::Retreat(retreat) => {
                let reverted = retreat.cursors.iter().filter_map(|cursor| Some(day::Reverted {
                    step: Seq::new(cursor.transition.application.get())?, unit: cursor.unit,
                })).collect();
                if retreat.profile != retreat.wanted { ApplicationKind::Fallback { events: retreat.events.clone(), reverted, wanted: retreat.wanted.clone() } }
                else { ApplicationKind::Retreat { events: retreat.events.clone(), reverted } }
            }
        })
    }

    pub(super) fn fold_climb_application(&mut self, event: &Event, applied: &journal::DayApplied) -> Result<(), FoldError> {
        let plan = match applied.cause {
            journal::ApplyCause::Step { decision } => Seq::new(decision.0).and_then(|seq| self.climb.plans.get(&seq)).cloned(),
            journal::ApplyCause::Reapplication {} | journal::ApplyCause::Reset { .. } => None,
        };
        if let Some(history) = &mut self.climb.history {
            let application = climb::ApplicationId::new(event.seq.get()).ok_or(FoldError::ClimbRecord("invalid application identity"))?;
            let target = UnitProfile::of_ccd(applied.target.ccd());
            if let Some(plan) = plan {
                if plan.target() != Some(&target) { return Err(FoldError::ClimbRecord("readback differs from decision")); }
                history.apply(application, event.time, &plan)?;
            } else if target != *history.current() {
                if !target.is_zero() { self.climb.campaign = climb::Campaign::Unreconstructible; }
                history.observe_application(application, target)?;
            }
        }
        self.observe_finishes(event.time);
        Ok(())
    }
}
