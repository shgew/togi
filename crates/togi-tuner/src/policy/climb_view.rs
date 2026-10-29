//! Journal-derived D status, with statistical S explicitly confined to diagnostics.

use super::climb_projection::{
    ClimbCampaignProjection, ClimbDailyEvidence, ClimbEventProjection, ClimbGoalSupport,
    ClimbHoldProjection, ClimbPermissionMode, ClimbProfiles, ClimbProjection,
    ClimbProposalProjection, ClimbQualificationProjection, ClimbQualificationState,
    ClimbStepProjection, ClimbUnitProjection, ClimbUnitState, DiagnosticStatisticalProjection,
};
use super::qualify::QualStatus;
use super::{CapProjection, State, ledger::saturating_u32, permission::vector};
use crate::{
    UnitProfile,
    cap::{Bound, Hours, Rate, poisson_upper},
    climb,
    day::{Invocation, Pause},
};
use togi_journal::{self as journal, Seq};

impl State {
    /// What blocks the next cold-boot decision now, by the same decision the day owes: `None` when it would
    /// not hold. The committed decisions are history; a night that maps an edge or completes a certificate
    /// after the last one changes the blockers without a new decision. The clock is the latest time any
    /// committed record carries, since a projection reads only the journal: a certificate older than its
    /// month by then no longer qualifies, as the day itself would refuse it.
    fn current_climb_blockers(&self) -> Option<Vec<journal::ClimbBlocker>> {
        let view = self.day_view().ok()?;
        let now = self.last_time.unwrap_or(jiff::Timestamp::MIN);
        let plan = self
            .climb_decision(
                &view,
                now,
                Invocation::ColdBoot {
                    pause: self
                        .days
                        .raw_intervals
                        .current()
                        .map_or(Pause::Running, |boot| self.promotion_pause(&boot.boot)),
                },
            )
            .ok()?;
        match &plan.action {
            climb::ClimbAction::Hold { blocked_by } => self.climb_blockers(blocked_by, &view).ok(),
            climb::ClimbAction::FirmwareZero { .. }
            | climb::ClimbAction::Retreat(_)
            | climb::ClimbAction::Restore(_)
            | climb::ClimbAction::Promote(_) => None,
        }
    }

    pub(super) fn climb_projection(&self) -> ClimbProjection {
        let Some(history) = &self.climb.history else {
            return ClimbProjection::default();
        };
        let ceilings = self.ceilings();
        let hold = self.credited();
        let candidate = self.day_view().ok().and_then(|view| {
            self.permission()
                .ok()?
                .candidate(&view.events)
                .ok()
                .flatten()
        });
        let candidate = candidate.as_ref().and_then(|profile| {
            Some(ClimbProposalProjection {
                profile: vector(profile, history.units()).ok()?,
                qualification: self.climb_qualification(profile),
            })
        });
        let latest = self.climb.decisions.values().next_back();
        let mut blockers = self.current_climb_blockers();
        let applied = vector(history.current(), history.units()).ok();
        let paused = self
            .days
            .raw_intervals
            .current()
            .filter(|boot| boot.pause().is_some());
        if paused.is_some() {
            let reasons = blockers.get_or_insert_with(Vec::new);
            if !reasons.contains(&journal::ClimbBlocker::Paused {}) {
                reasons.push(journal::ClimbBlocker::Paused {});
            }
        }
        let paused_saved = paused.and_then(|_| applied.clone());
        let paused_readback = paused.map(|boot| {
            let observed = self.days.boots.get(&boot.boot).and_then(|records| {
                records
                    .records
                    .iter()
                    .rev()
                    .find(|record| !matches!(record.what, super::books::Observed::CleanStop))
            });
            // Recording a pause precedes zeroing. A later successful application is a newer
            // readback than the observer's boot-start profile, not proof that zeroing happened.
            self.days
                .applications
                .last()
                .filter(|application| {
                    application.payload.boot == boot.boot
                        && observed.is_none_or(|record| application.seq > record.seq)
                })
                .map_or(boot.offsets, |application| application.payload.readbacks)
        });
        let observed = paused_readback.and_then(|offsets| {
            Self::observed_vector(
                &crate::day::CoreProfile::new(offsets).ok()?,
                history.units(),
            )
            .ok()
        });
        let units = history
            .units()
            .iter()
            .filter_map(|unit| {
                let depth = history.current().get(unit.id)?;
                let ceiling = ceilings.get(&unit.id).and_then(|ceiling| ceiling.c);
                let edge = self
                    .nights
                    .unit_edges
                    .get(&unit.id)
                    .filter(|_| ceilings.contains_key(&unit.id))
                    .map_or(journal::Edge::Unknown {}, |commit| {
                        commit.payload.new.clone()
                    });
                let state = if let Some(finish) = history.finishes().get(unit.id) {
                    ClimbUnitState::Finished {
                        reason: self.stop_reason(unit.id, &finish.reason).ok()?,
                        at: finish.at,
                        profile: vector(&finish.profile, history.units()).ok()?,
                        last_clean: finish.last_clean,
                        witnesses: self
                            .climb
                            .stops
                            .get(&unit.id)
                            .map_or_else(Vec::new, |stop| stop.payload.witnesses.clone()),
                    }
                } else if let Some(reason) = &blockers {
                    ClimbUnitState::Held {
                        reason: reason.clone(),
                    }
                } else {
                    ClimbUnitState::Climbing
                };
                let latest_step = self.days.applications.iter().rev().find_map(|application| {
                    let journal::ApplyCause::Step { decision } = application.payload.cause else {
                        return None;
                    };
                    let commitment = self.climb.decisions.get(&Seq::new(decision.0)?)?;
                    if application.payload.target.cores() != &commitment.payload.proposed.cores {
                        return None;
                    }
                    if commitment.payload.before.cores == commitment.payload.proposed.cores {
                        return None;
                    }
                    if !unit.cores.iter().any(|core| {
                        commitment.payload.before.cores.get(core)
                            != commitment.payload.proposed.cores.get(core)
                    }) {
                        return None;
                    }
                    Some(ClimbStepProjection {
                        application: application.payload.application,
                        at: application.time,
                        action: commitment.payload.decision.clone(),
                        before: commitment.payload.before.clone(),
                        after: commitment.payload.proposed.clone(),
                        witnesses: vec![commitment.seq, application.seq],
                    })
                });
                let diagnostic_s = unit
                    .id
                    .ccd()
                    .and_then(|ccd| self.nights.caps.get(&ccd))
                    .map(|cap| DiagnosticStatisticalProjection {
                        cap: CapProjection {
                            offset: cap.payload.new,
                            binding: cap.payload.binding.clone(),
                            slope: cap.payload.slope,
                            adequate: cap.payload.adequate.clone(),
                            uncovered: cap.payload.uncovered.clone(),
                            witness: cap.seq,
                        },
                    });
                let next = candidate
                    .as_ref()
                    .filter(|candidate| {
                        unit.cores.iter().any(|core| {
                            candidate.profile.cores.get(core)
                                != applied.as_ref().and_then(|applied| applied.cores.get(core))
                        })
                    })
                    .cloned();
                Some(ClimbUnitProjection {
                    unit: unit.id,
                    phase: history.units().phase(),
                    parent: unit.parent,
                    cores: unit.cores,
                    inherited_start: history
                        .ancestry(unit.id)
                        .first()
                        .map_or(togi_machine::TargetOffset::ZERO, |link| link.before),
                    applied: depth,
                    edge,
                    ceiling,
                    remaining: ceiling.map(|ceiling| {
                        saturating_u32(
                            depth
                                .get()
                                .saturating_sub(ceiling.get())
                                .max(0)
                                .unsigned_abs(),
                        )
                    }),
                    hold: ClimbHoldProjection {
                        credited_s: hold.1,
                        required_s: crate::day::HOLD_SECS,
                        witnesses: hold.0.clone(),
                    },
                    next,
                    state,
                    latest_step,
                    diagnostic_s,
                })
            })
            .collect();
        let read_back = self
            .days
            .applications
            .last()
            .map(|application| application.payload.readbacks);
        let read_back = paused_readback.or(read_back);
        let applied = observed.or(applied);
        ClimbProjection {
            permission: ClimbPermissionMode::ExperimentalHoldExactNightQualification,
            profiles: ClimbProfiles {
                applied: applied.clone(),
                read_back,
                proposed: latest.map(|commit| commit.payload.proposed.clone()),
                paused_saved,
            },
            units,
            campaign: self.climb_campaign_projection(),
            daily: applied.and_then(|applied| self.climb_daily_evidence(applied)),
            firmware_restoration: self
                .climb
                .restoration
                .as_ref()
                .map(|commit| commit.payload.clone()),
        }
    }

    fn climb_qualification(&self, profile: &UnitProfile) -> ClimbQualificationProjection {
        if let Some(witnesses) = self.qualification_blocked(profile) {
            return ClimbQualificationProjection {
                state: ClimbQualificationState::Failed {
                    witnesses: witnesses.clone(),
                },
                outstanding: Vec::new(),
                witnesses,
            };
        }
        let records = self.qualification_records();
        let record = records.iter().rev().find(|record| {
            profile.matches_ccd(record.start.payload.profile.ccd())
                && self.is_current_record(record)
        });
        let Some(record) = record else {
            return ClimbQualificationProjection {
                state: ClimbQualificationState::Missing,
                outstanding: Vec::new(),
                witnesses: Vec::new(),
            };
        };
        let status = self.qualification_status(record).ok();
        let (state, outstanding, witnesses) = match status.as_deref() {
            Some(QualStatus::Open {
                witnesses,
                outstanding,
                ..
            }) => (
                ClimbQualificationState::InProgress,
                outstanding.clone(),
                witnesses,
            ),
            Some(QualStatus::Complete { witnesses }) => (
                if matches!(
                    record.latest.payload.state,
                    journal::QualificationState::Complete { .. }
                ) {
                    ClimbQualificationState::Complete {
                        certificate: record.latest.seq,
                    }
                } else {
                    ClimbQualificationState::InProgress
                },
                Vec::new(),
                witnesses,
            ),
            Some(QualStatus::Failed {
                failure,
                witnesses,
                outstanding,
            }) => (
                ClimbQualificationState::Failed {
                    witnesses: vec![*failure],
                },
                outstanding.clone(),
                witnesses,
            ),
            Some(QualStatus::Stale) | None => {
                return ClimbQualificationProjection {
                    state: ClimbQualificationState::Missing,
                    outstanding: Vec::new(),
                    witnesses: Vec::new(),
                };
            }
        };
        ClimbQualificationProjection {
            state,
            outstanding,
            witnesses: witnesses
                .iter()
                .map(|witness| match witness.evidence {
                    journal::CellEvidence::Fresh { exposure }
                    | journal::CellEvidence::Reused { exposure } => exposure,
                })
                .collect(),
        }
    }

    fn climb_campaign_projection(&self) -> Option<ClimbCampaignProjection> {
        let start = self.climb.start.as_ref()?;
        let history = self.climb.history.as_ref()?;
        let view = self.day_view().ok()?;
        let mut events = Vec::new();
        for event in &view.events {
            let Some(id) = self.journal_event_id(&event.id) else {
                continue;
            };
            let charge = self
                .climb
                .campaign
                .charges()
                .iter()
                .find(|charge| charge.aliases.contains(&event.id));
            let Ok(profile) = Self::observed_vector(&event.profile, history.units()) else {
                continue;
            };
            let meta = view.meta.get(&event.id);
            events.push(ClimbEventProjection {
                event: id.clone(),
                aliases: charge.map_or_else(Vec::new, |charge| {
                    charge
                        .aliases
                        .iter()
                        .filter_map(|id| self.journal_event_id(id))
                        .collect()
                }),
                phase: charge.map_or(
                    journal::CampaignEventPhase::PostFinish,
                    |charge| match charge.phase {
                        climb::EventPhase::Learning => journal::CampaignEventPhase::Learning,
                        climb::EventPhase::PostFinish => journal::CampaignEventPhase::PostFinish,
                    },
                ),
                unit_phase: charge.map_or(history.units().phase(), |charge| charge.unit_phase),
                profile,
                provisional: charge.is_some_and(|charge| charge.provisional),
                classification: Self::journal_classification(event.class),
                reclassified: charge.is_some_and(|charge| charge.reclassified),
                witnesses: meta.map_or_else(Vec::new, |meta| meta.witnesses.clone()),
            });
        }
        for charge in self.climb.campaign.charges() {
            if view
                .events
                .iter()
                .any(|event| charge.aliases.contains(&event.id))
            {
                continue;
            }
            let receipt = self.climb.charges.iter().find(|receipt| {
                self.policy_event_id(&receipt.payload.event)
                    .is_some_and(|id| charge.aliases.contains(&id))
            });
            let Some(receipt) = receipt else {
                continue;
            };
            let mut witnesses = vec![receipt.seq];
            witnesses.extend(
                self.climb
                    .reclassifications
                    .iter()
                    .filter(|record| record.payload.event == receipt.payload.event)
                    .map(|record| record.seq),
            );
            events.push(ClimbEventProjection {
                event: receipt.payload.event.clone(),
                aliases: charge
                    .aliases
                    .iter()
                    .filter_map(|id| self.journal_event_id(id))
                    .collect(),
                phase: receipt.payload.phase,
                unit_phase: charge.unit_phase,
                profile: receipt.payload.profile.clone(),
                provisional: charge.provisional,
                classification: if charge.reclassified {
                    journal::Classification::NonCpu {}
                } else {
                    journal::Classification::CpuSuspect {
                        certainty: if charge.provisional {
                            journal::Certainty::Provisional
                        } else {
                            journal::Certainty::Definite
                        },
                    }
                },
                reclassified: charge.reclassified,
                witnesses,
            });
        }
        Some(ClimbCampaignProjection {
            campaign: start.payload.campaign,
            history: self.climb.history_error.as_ref().map_or(
                journal::CampaignHistory::Complete {},
                |reason| journal::CampaignHistory::Unreconstructible {
                    reason: reason.clone(),
                },
            ),
            limit: 2,
            consumed: saturating_u32(
                u64::try_from(self.climb.campaign.charges().len()).unwrap_or(u64::MAX),
            ),
            remaining: saturating_u32(
                u64::try_from(self.climb.campaign.remaining().unwrap_or(0)).unwrap_or(u64::MAX),
            ),
            provisional: saturating_u32(
                u64::try_from(
                    self.climb
                        .campaign
                        .charges()
                        .iter()
                        .filter(|charge| charge.provisional)
                        .count(),
                )
                .unwrap_or(u64::MAX),
            ),
            reclassified: saturating_u32(
                u64::try_from(self.climb.campaign.reclassified()).unwrap_or(u64::MAX),
            ),
            exhausted: self.climb.campaign.exhausted(),
            events,
        })
    }

    fn climb_daily_evidence(&self, profile: journal::UnitVector) -> Option<ClimbDailyEvidence> {
        let firmware = self.machine.firmware.clone()?;
        let since = self.machine.context_since;
        let matches = |observed: &journal::ObservedProfile| {
            let cores = match observed {
                journal::ObservedProfile::Ccd { cores, .. }
                | journal::ObservedProfile::PerCore { cores } => cores,
            };
            cores.len() == profile.cores.len()
                && profile
                    .cores
                    .iter()
                    .all(|(core, offset)| cores.get(core) == Some(offset.observed()))
        };
        let mut observed_s = 0_u64;
        let mut witnesses = Vec::new();
        for interval in &self.days.intervals {
            if interval.payload.state == journal::IntervalState::Applied
                && interval.payload.application.is_some()
                && interval
                    .payload
                    .checkpoints
                    .last()
                    .is_some_and(|seq| since.is_none_or(|since| *seq >= since))
                && matches(&interval.payload.profile)
            {
                observed_s = observed_s.saturating_add(
                    interval
                        .payload
                        .end_ns
                        .saturating_sub(interval.payload.start_ns)
                        / 1_000_000_000,
                );
                witnesses.push(interval.seq);
            }
        }
        let mut crashes = 0_u32;
        for event in self.days.events.values().filter(|event| {
            since.is_none_or(|since| event.first >= since) && matches(&event.latest.payload.profile)
        }) {
            crashes = crashes.saturating_add(1);
            witnesses.push(event.latest.seq);
        }
        let poisson_upper_per_h = Hours::new(f64::from(saturating_u32(observed_s)) / 3600.0)
            .ok()
            .and_then(|hours| match poisson_upper(crashes, hours) {
                Bound::Finite(rate) => Some(rate.get()),
                Bound::Unbounded => None,
            });
        Some(ClimbDailyEvidence {
            profile,
            firmware,
            epoch: crate::EVIDENCE_EPOCH,
            observed_s,
            clean_s: if crashes == 0 { observed_s } else { 0 },
            crashes,
            poisson_upper_per_h,
            goal: if poisson_upper_per_h.is_some_and(|rate| rate <= Rate::MACHINE_BUDGET.get()) {
                ClimbGoalSupport::MeetsGoal
            } else {
                ClimbGoalSupport::InsufficientEvidence
            },
            witnesses,
        })
    }
}
