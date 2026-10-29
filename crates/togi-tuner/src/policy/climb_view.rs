//! Journal-derived D status, with statistical S explicitly confined to diagnostics.

use togi_journal::{self as journal, Seq};
use crate::{UnitProfile, climb, cap::{Bound, Hours, Rate, poisson_upper}};
use super::{State, CapProjection, permission::vector, ledger::saturating_u32};
use super::climb_projection::{
    ClimbCampaignProjection, ClimbDailyEvidence, ClimbEventProjection, ClimbGoalSupport,
    ClimbHoldProjection, ClimbPermissionMode, ClimbProfiles, ClimbProjection,
    ClimbProposalProjection, ClimbQualificationProjection, ClimbQualificationState,
    ClimbStepProjection, ClimbUnitProjection, ClimbUnitState, DiagnosticStatisticalProjection,
};

impl State {
    pub(super) fn climb_projection(&self) -> ClimbProjection {
        let Some(history) = &self.climb.history else { return ClimbProjection::default(); };
        let ceilings = self.ceilings();
        let hold = self.credited();
        let candidate = self.day_view().ok().and_then(|view| self.permission().ok()?.candidate(&view.events).ok().flatten());
        let candidate = candidate.as_ref().and_then(|profile| Some(ClimbProposalProjection {
            profile: vector(profile, history.units()).ok()?, qualification: self.climb_qualification(profile),
        }));
        let latest = self.climb.decisions.values().next_back();
        let applied = vector(history.current(), history.units()).ok();
        let units = history.units().iter().filter_map(|unit| {
            let depth = history.current().get(unit.id)?;
            let ceiling = ceilings.get(&unit.id).and_then(|ceiling| ceiling.c);
            let edge = self.nights.unit_edges.get(&unit.id).filter(|_| ceilings.contains_key(&unit.id))
                .map_or(journal::Edge::Unknown {}, |commit| commit.payload.new.clone());
            let state = if let Some(finish) = history.finishes().get(unit.id) {
                ClimbUnitState::Finished {
                    reason: self.stop_reason(&finish.reason).ok()?, at: finish.at,
                    profile: vector(&finish.profile, history.units()).ok()?, last_clean: finish.last_clean,
                    witnesses: self.climb.stops.get(&unit.id).map_or_else(Vec::new, |stop| stop.payload.witnesses.clone()),
                }
            } else if let Some(journal::ClimbAction::Hold { blocked_by }) = latest.map(|commit| &commit.payload.decision) {
                ClimbUnitState::Held { reason: blocked_by.clone() }
            } else { ClimbUnitState::Climbing };
            let latest_step = self.days.applications.iter().rev().find_map(|application| {
                let journal::ApplyCause::Step { decision } = application.payload.cause else { return None; };
                let commitment = self.climb.decisions.get(&Seq::new(decision.0)?)?;
                if commitment.payload.before.cores == commitment.payload.proposed.cores { return None; }
                if !unit.cores.iter().any(|core| commitment.payload.before.cores.get(core) != commitment.payload.proposed.cores.get(core)) { return None; }
                Some(ClimbStepProjection {
                    application: application.payload.application, at: application.time,
                    action: commitment.payload.decision.clone(), before: commitment.payload.before.clone(), after: commitment.payload.proposed.clone(),
                    witnesses: vec![commitment.seq, application.seq],
                })
            });
            let diagnostic_s = unit.id.ccd().and_then(|ccd| self.nights.caps.get(&ccd)).map(|cap| DiagnosticStatisticalProjection {
                cap: CapProjection { offset: cap.payload.new, binding: cap.payload.binding.clone(), slope: cap.payload.slope,
                    adequate: cap.payload.adequate.clone(), uncovered: cap.payload.uncovered.clone(), witness: cap.seq },
            });
            let next = candidate.as_ref().filter(|candidate| unit.cores.iter().any(|core| {
                candidate.profile.cores.get(core) != applied.as_ref().and_then(|applied| applied.cores.get(core))
            })).cloned();
            Some(ClimbUnitProjection {
                unit: unit.id, phase: history.units().phase(), parent: unit.parent, cores: unit.cores,
                inherited_start: history.ancestry(unit.id).first().map_or(togi_machine::TargetOffset::ZERO, |link| link.before),
                applied: depth, edge, ceiling,
                remaining: ceiling.map(|ceiling| saturating_u32(depth.get().saturating_sub(ceiling.get()).max(0).unsigned_abs())),
                hold: ClimbHoldProjection { credited_s: hold.1, required_s: crate::day::HOLD_SECS, witnesses: hold.0.clone() },
                next, state, latest_step, diagnostic_s,
            })
        }).collect();
        let read_back = self.days.applications.last().map(|application| application.payload.readbacks);
        ClimbProjection {
            permission: ClimbPermissionMode::ExperimentalHoldExactNightQualification,
            profiles: ClimbProfiles { applied: applied.clone(), read_back, proposed: latest.map(|commit| commit.payload.proposed.clone()), paused_saved: None },
            units,
            campaign: self.climb_campaign_projection(),
            daily: applied.and_then(|applied| self.climb_daily_evidence(applied)),
            firmware_restoration: self.climb.restoration.as_ref().map(|commit| commit.payload.clone()),
        }
    }

    fn climb_qualification(&self, profile: &UnitProfile) -> ClimbQualificationProjection {
        if let Some(witnesses) = self.qualification_blocked(profile) {
            return ClimbQualificationProjection { state: ClimbQualificationState::Failed { witnesses: witnesses.clone() }, outstanding: Vec::new(), witnesses };
        }
        let records = self.qualification_records();
        let record = records.iter().rev().find(|record| profile.matches_ccd(record.start.payload.profile.ccd()) && self.is_current_record(record));
        let Some(record) = record else { return ClimbQualificationProjection { state: ClimbQualificationState::Missing, outstanding: Vec::new(), witnesses: Vec::new() }; };
        let (state, outstanding, cells) = match &record.latest.payload.state {
            journal::QualificationState::Start { outstanding } => (ClimbQualificationState::InProgress, outstanding.clone(), Vec::new()),
            journal::QualificationState::Progress { witnesses, outstanding } => (ClimbQualificationState::InProgress, outstanding.clone(), witnesses.iter().map(|witness| match witness.evidence { journal::CellEvidence::Fresh { exposure } | journal::CellEvidence::Reused { exposure } => exposure }).collect()),
            journal::QualificationState::Complete { witnesses } => (ClimbQualificationState::Complete { certificate: record.latest.seq }, Vec::new(), witnesses.iter().map(|witness| match witness.evidence { journal::CellEvidence::Fresh { exposure } | journal::CellEvidence::Reused { exposure } => exposure }).collect()),
            journal::QualificationState::Failed { failure, witnesses, outstanding } => (ClimbQualificationState::Failed { witnesses: vec![*failure] }, outstanding.clone(), witnesses.iter().map(|witness| match witness.evidence { journal::CellEvidence::Fresh { exposure } | journal::CellEvidence::Reused { exposure } => exposure }).collect()),
        };
        ClimbQualificationProjection { state, outstanding, witnesses: cells }
    }

    fn climb_campaign_projection(&self) -> Option<ClimbCampaignProjection> {
        let start = self.climb.start.as_ref()?;
        let history = self.climb.history.as_ref()?;
        let view = self.day_view().ok()?;
        let mut events = Vec::new();
        for event in &view.events {
            let Some(id) = self.journal_event_id(&event.id) else { continue; };
            let charge = self.climb.campaign.charges().iter().find(|charge| charge.aliases.contains(&event.id));
            let Ok(profile) = Self::observed_vector(&event.profile, history.units()) else { continue; };
            let meta = view.meta.get(&event.id);
            events.push(ClimbEventProjection {
                event: id.clone(), aliases: charge.map_or_else(Vec::new, |charge| charge.aliases.iter().filter_map(|id| self.journal_event_id(id)).collect()),
                phase: charge.map_or(journal::CampaignEventPhase::PostFinish, |charge| match charge.phase { climb::EventPhase::Learning => journal::CampaignEventPhase::Learning, climb::EventPhase::PostFinish => journal::CampaignEventPhase::PostFinish }),
                unit_phase: charge.map_or(history.units().phase(), |charge| charge.unit_phase),
                profile, provisional: charge.is_some_and(|charge| charge.provisional),
                classification: self.days.events.get(&id).map_or(journal::Classification::Unexplained {}, |book| book.latest.payload.classification),
                reclassified: charge.is_some_and(|charge| charge.reclassified), witnesses: meta.map_or_else(Vec::new, |meta| meta.witnesses.clone()),
            });
        }
        Some(ClimbCampaignProjection {
            campaign: start.payload.campaign, history: if self.climb.campaign.consumed().is_some() { journal::CampaignHistory::Complete {} } else { journal::CampaignHistory::Unreconstructible { reason: "The immutable campaign history cannot be reconstructed.".into() } },
            limit: 2, consumed: saturating_u32(u64::try_from(self.climb.campaign.charges().len()).unwrap_or(u64::MAX)),
            remaining: saturating_u32(u64::try_from(self.climb.campaign.remaining().unwrap_or(0)).unwrap_or(u64::MAX)),
            provisional: saturating_u32(u64::try_from(self.climb.campaign.charges().iter().filter(|charge| charge.provisional).count()).unwrap_or(u64::MAX)),
            reclassified: saturating_u32(u64::try_from(self.climb.campaign.reclassified()).unwrap_or(u64::MAX)), exhausted: self.climb.campaign.exhausted(), events,
        })
    }

    fn climb_daily_evidence(&self, profile: journal::UnitVector) -> Option<ClimbDailyEvidence> {
        let firmware = self.machine.firmware.clone()?;
        let since = self.machine.context_since;
        let matches = |observed: &journal::ObservedProfile| {
            let cores = match observed { journal::ObservedProfile::Ccd { cores, .. } | journal::ObservedProfile::PerCore { cores } => cores };
            cores.len() == profile.cores.len() && profile.cores.iter().all(|(core, offset)| cores.get(core) == Some(offset.observed()))
        };
        let mut observed_s = 0_u64;
        let mut witnesses = Vec::new();
        for interval in &self.days.intervals {
            if interval.payload.state == journal::IntervalState::Applied && interval.payload.application.is_some()
                && interval.payload.checkpoints.last().is_some_and(|seq| since.is_none_or(|since| *seq >= since)) && matches(&interval.payload.profile)
            {
                observed_s = observed_s.saturating_add(interval.payload.end_ns.saturating_sub(interval.payload.start_ns) / 1_000_000_000);
                witnesses.push(interval.seq);
            }
        }
        let mut crashes = 0_u32;
        for event in self.days.events.values().filter(|event| since.is_none_or(|since| event.first >= since) && matches(&event.latest.payload.profile)) {
            crashes = crashes.saturating_add(1); witnesses.push(event.latest.seq);
        }
        let poisson_upper_per_h = Hours::new(f64::from(saturating_u32(observed_s)) / 3600.0).ok().and_then(|hours| match poisson_upper(crashes, hours) { Bound::Finite(rate) => Some(rate.get()), Bound::Unbounded => None });
        Some(ClimbDailyEvidence {
            profile, firmware, epoch: crate::EVIDENCE_EPOCH, observed_s, clean_s: if crashes == 0 { observed_s } else { 0 }, crashes,
            poisson_upper_per_h, goal: if poisson_upper_per_h.is_some_and(|rate| rate <= Rate::MACHINE_BUDGET.get()) { ClimbGoalSupport::MeetsGoal } else { ClimbGoalSupport::InsufficientEvidence }, witnesses,
        })
    }
}
