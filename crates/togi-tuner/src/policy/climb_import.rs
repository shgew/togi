//! Original-namespace campaign reconstruction. Importing history never imports permission.

use togi_journal::{self as journal, Event, Payload, Provenance};

use crate::{climb, day::{self, EventId}};
use super::{State, FoldError, fold::committed, successor};

impl State {
    fn imported_identity(&self, id: &successor::Identity) -> EventId {
        match id {
            successor::Identity::Boot(boot) => EventId::Boot(boot.clone()),
            successor::Identity::Unobserved(at) => EventId::Unobserved(*at),
            successor::Identity::Record(original) => self.policy_event_id(&journal::DayEventId::Record { session: original.session, seq: original.seq }).unwrap_or(EventId::Imported(*original)),
            successor::Identity::Requalification { session, night } => EventId::Imported(Provenance { session: *session, seq: night.0 }),
        }
    }

    pub(super) fn fold_campaign_charge(&mut self, event: &Event, receipt: &journal::CampaignCharge) {
        if let Some(id) = self.policy_event_id(&receipt.event) {
            let aliases = receipt.aliases.iter().filter_map(|alias| self.policy_event_id(alias)).collect();
            self.climb.campaign.commit_charge(climb::Charge {
                event: id, aliases,
                phase: match receipt.phase { journal::CampaignEventPhase::Learning => climb::EventPhase::Learning, journal::CampaignEventPhase::PostFinish => climb::EventPhase::PostFinish },
                unit_phase: match receipt.unit_phase { journal::UnitPhase::Coarse => crate::Phase::Coarse, journal::UnitPhase::Core => crate::Phase::Core },
                provisional: receipt.provisional, reclassified: false,
            });
        }
        self.climb.charges.push(committed(event, receipt.clone()));
        self.observe_finishes(event.time);
    }

    pub(super) fn fold_campaign_reclassified(&mut self, event: &Event, receipt: &journal::CampaignReclassified) {
        let matching = self.climb.charges.iter().rev().find(|charge| charge.payload.event == receipt.event).map(|charge| charge.payload.clone());
        if let Some(charge) = matching
            && let Some(id) = self.policy_event_id(&receipt.event)
            && let Ok(profile) = day::CoreProfile::new(charge.profile.cores.iter().map(|(core, offset)| (core, togi_machine::ObservedOffset(offset.get()))).collect())
        {
            let class = match receipt.to {
                journal::Classification::NonCpu {} => day::Classification::NonCpu,
                journal::Classification::CpuSuspect { certainty: journal::Certainty::Provisional } => day::Classification::ProvisionalCpuSuspect(day::ProvisionalCause::UnansweredUnknown),
                journal::Classification::CpuSuspect { certainty: journal::Certainty::Definite } => day::Classification::CpuSuspect(day::CpuCause::CpuFaultEnd),
                journal::Classification::Unexplained {} => day::Classification::PendingQuestion,
            };
            self.climb.campaign.observe(&day::DailyEvent { id, profile, class, witnesses: Vec::new(), question: day::QuestionState::NotAsked }, &[], match charge.phase { journal::CampaignEventPhase::Learning => climb::EventPhase::Learning, journal::CampaignEventPhase::PostFinish => climb::EventPhase::PostFinish }, match charge.unit_phase { journal::UnitPhase::Coarse => crate::Phase::Coarse, journal::UnitPhase::Core => crate::Phase::Core });
        }
        self.climb.reclassifications.push(committed(event, receipt.clone()));
    }

    pub(super) fn reconstruct_campaign(&mut self, at: jiff::Timestamp) -> Result<(), FoldError> {
        if self.climb.imports.is_empty() && self.climb.import_summary.is_none() { return Ok(()); }
        let unknown = self.climb.import_summary.as_ref().map_or(&[][..], |summary| summary.unknown.as_slice());
        let reconstruction = successor::reconstruct(&self.climb.imports, unknown);
        let successor::Reconstruction::Complete(reconstructed) = reconstruction else {
            if let successor::Reconstruction::Unreconstructible { reason } = reconstruction { self.climb.history_error.get_or_insert(reason); }
            return Ok(());
        };
        if let Some(campaign) = reconstructed.campaign
            && self.climb.start.as_ref().is_some_and(|start| start.payload.campaign != campaign)
        { self.climb.history_error.get_or_insert_with(|| "The successor changed the original campaign identity.".into()); }
        for charge in &reconstructed.charges {
            let event = self.imported_identity(&charge.event);
            let aliases = charge.aliases.iter().map(|alias| self.imported_identity(alias)).collect::<std::collections::BTreeSet<_>>();
            let kept = self.climb.campaign.charges().iter().find(|kept| !kept.aliases.is_disjoint(&aliases));
            let receipt = charge.witnesses.iter().find_map(|witness| {
                self.climb.imports.iter().find(|record| record.original == *witness)
                    .and_then(|record| successor::decode(record).ok())
                    .and_then(|event| if let Payload::CampaignCharge(receipt) = event.payload { Some(receipt) } else { None })
            });
            let phase = receipt.as_ref().map_or_else(|| kept.map_or(climb::EventPhase::Learning, |kept| kept.phase), |receipt| match receipt.phase { journal::CampaignEventPhase::Learning => climb::EventPhase::Learning, journal::CampaignEventPhase::PostFinish => climb::EventPhase::PostFinish });
            let unit_phase = receipt.as_ref().map_or_else(|| kept.map_or(crate::Phase::Coarse, |kept| kept.unit_phase), |receipt| match receipt.unit_phase { journal::UnitPhase::Coarse => crate::Phase::Coarse, journal::UnitPhase::Core => crate::Phase::Core });
            let profile = receipt.map(|receipt| receipt.profile).or_else(|| {
                let history = self.climb.history.as_ref()?;
                let observed = match &charge.profile { journal::ObservedProfile::Ccd { cores, .. } | journal::ObservedProfile::PerCore { cores } => day::CoreProfile::new(*cores).ok()? };
                Self::observed_vector(&observed, history.units()).ok()
            });
            let causes = charge.witnesses.iter().filter_map(|original| self.climb.import_sequences.get(original).copied()).collect::<Vec<_>>();
            if let Some(seq) = causes.first().copied()
                && let Some(profile) = profile
            {
                self.climb.imported_observations.insert(event.clone(), super::books::Committed { seq, time: at, causes, payload: profile });
            } else {
                self.climb.history_error.get_or_insert_with(|| "An imported charge has no reconstructible profile and original witness.".into());
            }
            self.climb.campaign.commit_charge(climb::Charge { event, aliases, phase, unit_phase, provisional: charge.provisional, reclassified: charge.reclassified });
        }
        for stop in reconstructed.stops.values() {
            if let Some(&seq) = self.climb.import_sequences.get(&stop.witness) {
                self.climb.imported_stop_witnesses.insert(stop.unit, seq);
            } else {
                self.climb.history_error.get_or_insert_with(|| "A predecessor stop has no imported original witness.".into());
            }
            let original = self.climb.imports.iter().find(|record| record.original == stop.witness);
            let decoded = original.and_then(|record| successor::decode(record).ok());
            let (reason, profile) = if let Some(Event { payload: Payload::UnitStop(record), .. }) = decoded {
                (self.finish_reason(&record.reason, stop.witness.seq)?, self.unit_profile(&record.profile)?)
            } else {
                let Some(history) = &self.climb.history else { continue; };
                let Some(charge) = reconstructed.charges.iter().find(|charge| charge.witnesses.contains(&stop.witness)) else {
                    self.climb.history_error.get_or_insert_with(|| "A predecessor stop has no classified event witness.".into());
                    continue;
                };
                let observed = match &charge.profile { journal::ObservedProfile::Ccd { cores, .. } | journal::ObservedProfile::PerCore { cores } => day::CoreProfile::new(*cores) };
                let profile = observed.ok().and_then(|observed| Self::observed_vector(&observed, history.units()).ok()).and_then(|vector| self.unit_profile(&vector).ok());
                let Some(profile) = profile else {
                    self.climb.history_error.get_or_insert_with(|| "A predecessor stop has no reconstructible applied profile.".into());
                    continue;
                };
                (climb::FinishReason::Stopped(climb::StopReason::Predecessor { original: stop.witness }), profile.with(stop.unit, stop.last_clean))
            };
            if let Some(history) = &mut self.climb.history {
                if history.units().get(stop.unit).is_none() {
                    self.climb.history_error.get_or_insert_with(|| "A predecessor stop names an unavailable unit mapping.".into());
                } else {
                    history.finishes_mut().stop(stop.unit, climb::FinishWitness { reason, at, profile, last_clean: stop.last_clean, ceiling: None });
                }
            }
        }
        Ok(())
    }
}
