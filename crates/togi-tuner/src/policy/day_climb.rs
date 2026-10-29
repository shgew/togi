//! Durable explanations of the pure climb, never hardware applications.

use togi_journal::{self as journal, Seq};
use crate::{UnitProfile, climb, day::{EventId, Invocation, QuestionState}};
use super::{State, DayQuery, Decision, PolicyPayload, NotReady, day_events::DayView, permission::{mapping, vector}};

impl State {
    pub(super) fn climb_hold(&self) -> Result<journal::ClimbHold, NotReady> {
        let history = self.days.history.as_ref().ok_or(NotReady::NoBaseline)?;
        let application = self.days.applications.iter().rev()
            .find(|application| application.seq <= history.current().at)
            .map_or(journal::ApplicationId(history.current().at.get()), |application| application.payload.application);
        let (credited, eligible_s) = self.credited();
        Ok(journal::ClimbHold { application, credited, eligible_s })
    }

    pub(super) fn owed_climb_commitment(&self, query: &DayQuery) -> Result<Option<Decision>, NotReady> {
        let Some(history) = self.climb.history.as_ref() else { return Ok(None); };
        let units = history.units();
        let current_mapping = mapping(units);
        if self.climb.mapping.as_ref().map(|commit| &commit.payload) != Some(&current_mapping) {
            return Ok(Some(Decision::new(PolicyPayload::UnitMapping(current_mapping), Vec::new())));
        }
        if self.climb.start.is_none() {
            let session = self.machine.session.ok_or(NotReady::NoSession)?;
            let seq = self.last.and_then(|seq| Seq::new(seq.get().checked_add(1)?)).ok_or(NotReady::SeqExhausted)?;
            let imported = !self.climb.imports.is_empty() || self.climb.import_summary.is_some();
            let history = if imported {
                journal::CampaignHistory::Unreconstructible { reason: "predecessor campaign history has not been reconstructed".into() }
            } else { journal::CampaignHistory::Complete {} };
            return Ok(Some(Decision::new(PolicyPayload::CampaignStart(journal::CampaignStart {
                campaign: journal::CampaignId(journal::Provenance { session, seq }),
                allowance: 2,
                origin: journal::CampaignOrigin::Fresh {},
                history,
            }), Vec::new())));
        }
        if let Some(charge) = self.owed_campaign_charge()? { return Ok(Some(charge)); }
        for (unit, _) in history.current().iter() {
            let Some(finish) = history.finishes().get(unit) else { continue; };
            let reason = self.stop_reason(&finish.reason)?;
            let profile = vector(&finish.profile, units)?;
            let same = self.climb.stops.get(&unit).is_some_and(|commit| commit.payload.reason == reason);
            if !same {
                let mut witnesses = self.nights.unit_edges.get(&unit).map(|commit| commit.seq).into_iter().collect::<Vec<_>>();
                if let climb::FinishReason::Stopped(climb::StopReason::FirstBackstep { event, .. }
                    | climb::StopReason::RestorationBackstep { event, .. }) = &finish.reason
                    && let Some(id) = self.journal_event_id(event)
                    && let Some(event) = self.days.events.get(&id)
                { witnesses.push(event.latest.seq); }
                witnesses.extend(self.days.applications.last().map(|commit| commit.seq));
                return Ok(Some(Decision::new(PolicyPayload::UnitStop(journal::UnitStop {
                    unit, reason, profile, last_clean: finish.last_clean, witnesses: witnesses.clone(),
                }), witnesses)));
            }
        }
        let saved = if let Some(saved) = history.saved() {
            Some(saved.clone())
        } else if self.firmware_state() == crate::day::Firmware::Changed {
            Some(climb::SavedProfile { units: units.clone(), profile: history.current().clone() })
        } else { None };
        if let Some(saved) = saved.filter(|saved| !saved.profile.is_zero()) {
            let saved = vector(&saved.profile, &saved.units)?;
            let state = if let Some(certificate) = self.qualification_seq_for_vector(&saved) {
                let hold = self.climb_hold()?;
                if hold.eligible_s >= crate::day::HOLD_SECS {
                    journal::FirmwareRestorationState::Held { hold }
                } else { journal::FirmwareRestorationState::Qualified { certificate } }
            } else { journal::FirmwareRestorationState::Saved {} };
            let desired = journal::FirmwareRestoration { saved, state };
            if self.climb.restoration.as_ref().map(|commit| &commit.payload) != Some(&desired) {
                return Ok(Some(Decision::new(PolicyPayload::FirmwareRestoration(desired), Vec::new())));
            }
        } else if let Some(commit) = &self.climb.restoration {
            let restored = history.steps().iter().rev().find(|step| step.kind == climb::StepKind::FirmwareRestoration);
            if let Some(step) = restored {
                let state = history.retreats().iter().rev().find_map(|retreat| {
                    retreat.cursors.iter().any(|cursor| cursor.transition.application == step.application)
                        .then(|| retreat.events.first()).flatten()
                }).and_then(|event| self.journal_event_id(event))
                    .map_or_else(|| journal::FirmwareRestorationState::Restored {
                        application: self.days.applications.iter().find(|commit| commit.seq.get() == step.application.get())
                            .map_or(journal::ApplicationId(step.application.get()), |commit| commit.payload.application),
                    }, |event| journal::FirmwareRestorationState::SteppedBack { event });
                if commit.payload.state != state {
                    return Ok(Some(Decision::new(PolicyPayload::FirmwareRestoration(journal::FirmwareRestoration {
                        saved: commit.payload.saved.clone(), state,
                    }), vec![commit.seq])));
                }
            }
        }
        let _ = query;
        Ok(None)
    }

    pub(super) fn stop_reason(&self, reason: &climb::FinishReason) -> Result<journal::UnitStopReason, NotReady> {
        Ok(match reason {
            climb::FinishReason::Ceiling => journal::UnitStopReason::Ceiling {},
            climb::FinishReason::AllowanceExhausted => journal::UnitStopReason::AllowanceExhausted {},
            climb::FinishReason::Stopped(reason) => match reason {
                climb::StopReason::AdverseCeiling => journal::UnitStopReason::AdverseCeiling {},
                climb::StopReason::Veto => journal::UnitStopReason::Veto {},
                climb::StopReason::FirstBackstep { event, application } => {
                    let event = self.journal_event_id(event).ok_or(NotReady::NoSession)?;
                    let application = application.and_then(|id| self.days.applications.iter().find(|commit| commit.seq.get() == id.get()))
                        .or_else(|| self.days.applications.last())
                        .map(|commit| commit.payload.application).ok_or(NotReady::NoBaseline)?;
                    journal::UnitStopReason::FirstBackstep { event, application }
                }
                climb::StopReason::RestorationBackstep { event, .. } => journal::UnitStopReason::RestorationBackstep {
                    event: self.journal_event_id(event).ok_or(NotReady::NoSession)?,
                },
            },
        })
    }

    fn owed_campaign_charge(&self) -> Result<Option<Decision>, NotReady> {
        let Some(start) = &self.climb.start else { return Ok(None); };
        let view = self.day_view()?;
        for (index, charge) in self.climb.campaign.charges().iter().enumerate() {
            let Some(event) = self.journal_event_id(&charge.event) else { continue; };
            let kept = self.climb.charges.iter().find(|commit| commit.payload.event == event);
            if let Some(kept) = kept {
                let latest = self.days.events.get(&event).map(|book| &book.latest);
                if let Some(latest) = latest {
                    let from = self.climb.reclassifications.iter().rev().find(|commit| commit.payload.event == event)
                        .map_or(journal::Classification::CpuSuspect { certainty: if charge.provisional { journal::Certainty::Provisional } else { journal::Certainty::Definite } }, |commit| commit.payload.to);
                    let to = latest.payload.classification;
                    if from != to {
                        return Ok(Some(Decision::new(PolicyPayload::CampaignReclassified(journal::CampaignReclassified {
                            campaign: start.payload.campaign, event, from, to,
                        }), vec![kept.seq, latest.seq])));
                    }
                }
                continue;
            }
            let Some(observed) = view.events.iter().find(|event| event.id == charge.event) else { continue; };
            let history = self.climb.history.as_ref().ok_or(NotReady::NoBaseline)?;
            let profile = self.profile_of_observed(&observed.profile, history.units())?;
            let consumed = u32::try_from(index + 1).map_err(|_| NotReady::SeqExhausted)?;
            let aliases = charge.aliases.iter().filter_map(|id| self.journal_event_id(id)).collect();
            let causes = self.days.events.get(&event).map(|book| book.latest.seq).into_iter().collect();
            return Ok(Some(Decision::new(PolicyPayload::CampaignCharge(journal::CampaignCharge {
                campaign: start.payload.campaign, event, aliases,
                profile: vector(&profile, history.units())?,
                phase: match charge.phase { climb::EventPhase::Learning => journal::CampaignEventPhase::Learning, climb::EventPhase::PostFinish => journal::CampaignEventPhase::PostFinish },
                unit_phase: match charge.unit_phase { crate::Phase::Coarse => journal::UnitPhase::Coarse, crate::Phase::Core => journal::UnitPhase::Core },
                provisional: charge.provisional, consumed, remaining: 2_u32.saturating_sub(consumed), exhausted: consumed >= 2,
            }), causes)));
        }
        Ok(None)
    }

    pub(super) fn profile_of_observed(&self, observed: &crate::day::CoreProfile, units: &crate::Units) -> Result<UnitProfile, NotReady> {
        let mut values = Vec::new();
        for unit in units.iter() {
            let mut offsets = unit.cores.iter().filter_map(|core| observed.offsets().get(core));
            let first = offsets.next().ok_or(NotReady::NoBaseline)?;
            if offsets.any(|offset| offset != first) { return Err(NotReady::NonCoarse); }
            values.push((unit.id, togi_machine::TargetOffset::new(first.0).map_err(crate::day::DayStepError::from)?));
        }
        Ok(UnitProfile::try_from(values)?)
    }

    pub(super) fn qualification_seq_for_vector(&self, vector: &journal::UnitVector) -> Option<Seq> {
        self.qualification_records().iter().filter(|record| {
            record.start.payload.profile.cores() == &vector.cores && self.is_current_record(record)
                && matches!(record.latest.payload.state, journal::QualificationState::Complete { .. })
        }).map(|record| record.latest.seq).max()
    }

    fn climb_blockers(&self, blocked: &[climb::Blocker], view: &DayView) -> Result<Vec<journal::ClimbBlocker>, NotReady> {
        let history = self.climb.history.as_ref().ok_or(NotReady::NoBaseline)?;
        let mut result = Vec::new();
        for blocker in blocked {
            match blocker {
                climb::Blocker::Hold { credited_secs, required_secs } => result.push(journal::ClimbBlocker::Hold { credited_s: *credited_secs, required_s: *required_secs }),
                climb::Blocker::HoldProfile => result.push(journal::ClimbBlocker::HoldProfile {}),
                climb::Blocker::Certificate => result.push(journal::ClimbBlocker::Certificate {}),
                climb::Blocker::FailedCertificate => {
                    if let Some(profile) = self.permission()?.candidate(&view.events)? {
                        result.push(journal::ClimbBlocker::CertificateFailed { witnesses: self.qualification_blocked(&profile).unwrap_or_default(), profile: vector(&profile, history.units())? });
                    }
                }
                climb::Blocker::PendingQuestion => result.extend(view.events.iter().filter(|event| event.question == QuestionState::Pending).filter_map(|event| {
                    if let EventId::Boot(boot) = &event.id { Some(journal::ClimbBlocker::QuestionPending { boot: boot.clone() }) } else { None }
                })),
                climb::Blocker::UnresolvedEvent => {
                    let statuses = self.statuses(view)?.unwrap_or_default();
                    result.extend(statuses.iter().filter(|status| !status.is_resolved()).filter_map(|status| self.journal_event_id(&status.id)).map(|event| journal::ClimbBlocker::EventUnresolved { event }));
                }
                climb::Blocker::Veto => result.extend(crate::day::vetoes(&view.events).iter().filter_map(|veto| self.journal_event_id(&veto.event)).map(|event| journal::ClimbBlocker::Veto { event })),
                climb::Blocker::FirmwareInvalid => result.push(journal::ClimbBlocker::FirmwareInvalid {}),
                climb::Blocker::Freeze => result.push(journal::ClimbBlocker::Frozen {}),
                climb::Blocker::Pause => result.push(journal::ClimbBlocker::Paused {}),
                climb::Blocker::SameBoot => result.push(journal::ClimbBlocker::SameBoot {}),
                climb::Blocker::AllowanceExhausted => result.push(journal::ClimbBlocker::AllowanceExhausted {}),
                climb::Blocker::CampaignHistory => result.push(journal::ClimbBlocker::CampaignHistory {}),
                climb::Blocker::UnknownEdge { unit } => result.push(journal::ClimbBlocker::UnknownEdge { unit: *unit }),
                climb::Blocker::Ceiling { unit } => result.push(journal::ClimbBlocker::Finished { unit: *unit }),
                climb::Blocker::Finished => result.extend(history.current().units().map(|unit| journal::ClimbBlocker::Finished { unit })),
                climb::Blocker::ActivationClosed => result.push(journal::ClimbBlocker::ActivationClosed {}),
                climb::Blocker::SafeApply => result.push(journal::ClimbBlocker::SafeApply {}),
            }
        }
        Ok(result)
    }

    pub(super) fn owed_climb_decision(&self, query: &DayQuery, view: &DayView) -> Result<Option<Decision>, NotReady> {
        let Some(history) = self.climb.history.as_ref() else { return Ok(None); };
        let plan = self.climb_decision(view, query.now, query.invocation)?;
        let issued = self.climb.decisions.values().rev().find(|commit| commit.payload.cold_boot == query.boot);
        if matches!(plan.action, climb::ClimbAction::Hold { .. }) && matches!(query.invocation, Invocation::SameBoot) { return Ok(None); }
        if issued.is_some() && matches!(plan.action, climb::ClimbAction::Hold { .. } | climb::ClimbAction::Promote(_) | climb::ClimbAction::Restore(_)) { return Ok(None); }
        let proposed = vector(plan.target().unwrap_or(history.current()), history.units())?;
        if issued.is_some_and(|commit| commit.payload.proposed == proposed && commit.payload.before == vector(&plan.before, history.units()).unwrap_or_else(|_| proposed.clone())) { return Ok(None); }
        let changed = proposed.mapping.units.iter().filter(|unit| unit.cores.iter().any(|core| proposed.cores.get(core) != history.current().expand(history.units()).ok().and_then(|cores| cores.get(core)))).map(|unit| unit.unit).collect::<Vec<_>>();
        let edges = self.nights.unit_edges.values().filter(|commit| self.ceilings().contains_key(&commit.payload.unit)).map(|commit| commit.seq).collect::<Vec<_>>();
        let events = self.days.events.values().map(|book| book.latest.seq).collect::<Vec<_>>();
        let action = match &plan.action {
            climb::ClimbAction::Hold { blocked_by } => journal::ClimbAction::Hold { blocked_by: self.climb_blockers(blocked_by, view)? },
            climb::ClimbAction::Promote(proposal) => journal::ClimbAction::Promote { unit: proposal.unit, from: proposal.from, to: proposal.to, step: u32::try_from(proposal.step).map_err(|_| NotReady::SeqExhausted)? },
            climb::ClimbAction::FirmwareZero { .. } => journal::ClimbAction::FirmwareZero {},
            climb::ClimbAction::Restore(proposal) => journal::ClimbAction::Restore { restoration: match proposal.kind { climb::RestorationKind::Firmware => journal::RestorationKind::Firmware, climb::RestorationKind::Reclassified { .. } => journal::RestorationKind::Reclassified }, units: changed },
            climb::ClimbAction::Retreat(retreat) => journal::ClimbAction::Retreat {
                units: changed,
                cause: if !retreat.adverse_units.is_empty() { journal::ClimbRetreatCause::AdverseCeiling { edges: edges.clone() } }
                    else if let Some(event) = retreat.events.first().and_then(|event| self.journal_event_id(event)) { journal::ClimbRetreatCause::Event { event } }
                    else { journal::ClimbRetreatCause::Completion {} },
                wanted: (retreat.profile != retreat.wanted).then(|| vector(&retreat.wanted, history.units())).transpose()?,
            },
        };
        let certificate = self.qualification_seq_for_vector(&proposed);
        let hold = self.climb_hold()?;
        let mut causes = edges.clone(); causes.extend(events.iter().copied()); causes.extend(certificate); causes.extend(hold.credited.iter().copied());
        causes.sort_unstable(); causes.dedup();
        Ok(Some(Decision::new(PolicyPayload::ClimbDecision(journal::ClimbDecision {
            decision: action, cold_boot: query.boot.clone(), before: vector(&plan.before, history.units())?, proposed, hold, certificate, ceilings: edges, events,
        }), causes)))
    }
}
