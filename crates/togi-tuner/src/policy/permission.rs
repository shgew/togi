//! The shared D candidate and hard ceilings. Statistical S is never an input.

use super::{State, day_events::DayView, night_query::NotReady};
use crate::{
    UnitProfile, climb,
    day::{DailyEvent, Invocation, QuestionState, vetoes},
};
use togi_journal::{Edge, UnitMapping, UnitMembership, UnitPhase, UnitVector};

pub(super) struct Permission<'a> {
    pub(super) history: &'a climb::ClimbHistory,
    pub(super) ceilings: climb::Ceilings,
}

impl Permission<'_> {
    pub(super) fn candidate(&self, events: &[DailyEvent]) -> Result<Option<UnitProfile>, NotReady> {
        if let Some(restoration) = climb::restoration_candidate(self.history, events)? {
            return Ok(Some(restoration.profile));
        }
        Ok(climb::propose(
            self.history.current(),
            &self.ceilings,
            self.history.finishes(),
        )
        .map(|proposal| proposal.profile))
    }

    pub(super) fn clip(&self, wanted: &UnitProfile) -> Result<UnitProfile, NotReady> {
        Ok(UnitProfile::try_from(
            wanted
                .iter()
                .map(|(unit, offset)| {
                    let permitted = self.ceilings.get(&unit).and_then(|ceiling| ceiling.c);
                    (
                        unit,
                        permitted.map_or(offset, |ceiling| offset.max(ceiling)),
                    )
                })
                .collect::<Vec<_>>(),
        )?)
    }
}

pub(super) fn mapping(units: &crate::Units) -> UnitMapping {
    UnitMapping {
        phase: match units.phase() {
            crate::Phase::Coarse => UnitPhase::Coarse,
            crate::Phase::Core => UnitPhase::Core,
        },
        units: units
            .iter()
            .map(|unit| UnitMembership {
                unit: unit.id,
                cores: unit.cores,
                parent: unit.parent,
            })
            .collect(),
    }
}

pub(super) fn vector(profile: &UnitProfile, units: &crate::Units) -> Result<UnitVector, NotReady> {
    Ok(UnitVector {
        cores: profile.expand(units)?,
        mapping: mapping(units),
    })
}

pub(super) fn ceiling(edge: &Edge) -> climb::Ceiling {
    let state = match edge {
        Edge::Unknown {} => climb::EdgeState::Unknown,
        Edge::Observed { offset, witness } => match climb::EvidenceId::new(witness.get()) {
            Some(witness) => climb::EdgeState::Observed {
                offset: *offset,
                witness,
            },
            None => climb::EdgeState::Unknown,
        },
        Edge::Censored { witnesses, .. } => climb::EdgeState::Censored {
            witnesses: witnesses
                .iter()
                .filter_map(|seq| climb::EvidenceId::new(seq.get()))
                .collect(),
        },
    };
    climb::Ceiling::new(state)
}

impl State {
    pub(super) fn ceilings(&self) -> climb::Ceilings {
        self.nights
            .unit_edges
            .iter()
            .filter(|(_, commit)| {
                self.machine.firmware.as_ref() == Some(&commit.payload.firmware)
                    && commit.payload.epoch == crate::EVIDENCE_EPOCH
                    && self.machine.context_since.is_none_or(|since| {
                        if commit.seq >= since {
                            return true;
                        }
                        let current = |witness: &togi_journal::Seq| {
                            self.trials.all.iter().any(|trial| {
                                trial.end.as_ref().is_some_and(|end| &end.seq == witness)
                                    && self.is_current_trial(trial)
                            })
                        };
                        match &commit.payload.new {
                            Edge::Unknown {} => true,
                            Edge::Observed { witness, .. } => current(witness),
                            Edge::Censored { witnesses, .. } => {
                                !witnesses.is_empty() && witnesses.iter().all(current)
                            }
                        }
                    })
            })
            .map(|(&unit, commit)| (unit, ceiling(&commit.payload.new)))
            .collect()
    }

    pub(super) fn permission(&self) -> Result<Permission<'_>, NotReady> {
        Ok(Permission {
            history: self.climb.history.as_ref().ok_or(NotReady::NoBaseline)?,
            ceilings: self.ceilings(),
        })
    }

    /// All precedence and exact-profile blockers enter the same pure decision engine.
    pub(super) fn climb_decision(
        &self,
        view: &DayView,
        now: jiff::Timestamp,
        invocation: Invocation,
    ) -> Result<climb::ClimbDecision, NotReady> {
        let permission = self.permission()?;
        let qualified = self.qualified_profiles(now);
        let candidate = permission.candidate(&view.events)?;
        let mut checked = qualified.clone();
        checked.extend(candidate);
        let failed = checked
            .into_iter()
            .filter(|profile| self.qualification_blocked(profile).is_some())
            .collect();
        let vetoes = vetoes(&view.events);
        let statuses = self.statuses(view)?.unwrap_or_default();
        let (_, seconds) = self.credited();
        Ok(climb::decide(&climb::ClimbInputs {
            history: permission.history,
            campaign: if self.climb.start.is_some() && self.climb.history_error.is_none() {
                &self.climb.campaign
            } else {
                &climb::Campaign::Unreconstructible
            },
            ceilings: &permission.ceilings,
            events: &view.events,
            vetoes: &vetoes,
            qualified: &qualified,
            failed: &failed,
            hold: climb::HoldInput {
                profile: permission.history.current(),
                credited_secs: seconds,
            },
            firmware: self.firmware_state(),
            invocation,
            freeze: self.days.freeze,
            pending_question: view
                .events
                .iter()
                .any(|event| event.question == QuestionState::Pending),
            unresolved_event: statuses.iter().any(|status| !status.is_resolved()),
            // Activation and the hardware transaction are enforced by the stock-only applier,
            // not by hiding a permitted experimental proposal from the pure tuner.
            apply: climb::ApplyPrerequisites {
                firmware_valid: self.machine.firmware.is_some() && self.stop.is_none(),
                activation_open: true,
                safe_apply: true,
            },
        })?)
    }
}
