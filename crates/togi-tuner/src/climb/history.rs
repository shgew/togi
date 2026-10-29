use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use togi_machine::{PerCore, TargetOffset};

use crate::day::{CoreProfile, CoreProfileError, EventId};
use crate::unit::{UnitId, UnitProfile, UnitProfileError, Units};

use super::{
    ClimbAction, ClimbDecision, FinishBook, FinishReason, FinishWitness, RestorationKind,
    SavedProfile, StopReason,
};

#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct ApplicationId(u64);

impl ApplicationId {
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One actual successful transition, referenced rather than copied when units subdivide.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct TransitionId {
    pub application: ApplicationId,
    pub source: UnitId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AncestryLink {
    pub transition: TransitionId,
    pub before: TargetOffset,
    pub after: TargetOffset,
}

/// A current unit's consumed reference to a real transition. Siblings can reference one parent
/// transition without creating extra learning steps or event charges.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct Cursor {
    pub unit: UnitId,
    pub transition: TransitionId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StepKind {
    Ordinary,
    FirmwareRestoration,
    ReclassificationRestoration,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LearningStep {
    pub application: ApplicationId,
    pub at: Timestamp,
    pub before: SavedProfile,
    pub after: SavedProfile,
    pub changed: Vec<UnitId>,
    pub kind: StepKind,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RetreatRecord {
    pub application: ApplicationId,
    pub units: Units,
    pub restorable: bool,
    pub before: UnitProfile,
    pub after: UnitProfile,
    pub events: Vec<EventId>,
    pub cursors: Vec<Cursor>,
    pub wanted: UnitProfile,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClimbError {
    #[error("expanding the tuning profile")]
    Profile(#[from] UnitProfileError),
    #[error("constructing the actual core profile")]
    CoreProfile(#[from] CoreProfileError),
    #[error("application does not follow the successful history")]
    ApplicationOrder,
    #[error("decision does not start at the applied profile")]
    StaleDecision,
    #[error("refinement does not preserve the applied physical profile")]
    Refinement,
    #[error("hold is not an application")]
    HoldApplication,
}

/// Successful applications alone advance ancestry, cursors and permanent stops.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClimbHistory {
    units: Units,
    current: UnitProfile,
    latest: Option<ApplicationId>,
    steps: Vec<LearningStep>,
    #[serde(with = "crate::policy::wire::pairs")]
    links: BTreeMap<UnitId, Vec<AncestryLink>>,
    consumed: BTreeSet<Cursor>,
    retreats: Vec<RetreatRecord>,
    finishes: FinishBook,
    saved: Option<SavedProfile>,
    restored_reclassifications: BTreeSet<ApplicationId>,
}

impl ClimbHistory {
    pub fn stock(units: Units) -> Self {
        Self {
            current: UnitProfile::zero(&units),
            units,
            latest: None,
            steps: Vec::new(),
            links: BTreeMap::new(),
            consumed: BTreeSet::new(),
            retreats: Vec::new(),
            finishes: FinishBook::default(),
            saved: None,
            restored_reclassifications: BTreeSet::new(),
        }
    }

    pub const fn units(&self) -> &Units {
        &self.units
    }
    pub const fn current(&self) -> &UnitProfile {
        &self.current
    }
    pub fn steps(&self) -> &[LearningStep] {
        &self.steps
    }
    pub fn retreats(&self) -> &[RetreatRecord] {
        &self.retreats
    }
    pub const fn finishes(&self) -> &FinishBook {
        &self.finishes
    }
    pub fn finishes_mut(&mut self) -> &mut FinishBook {
        &mut self.finishes
    }
    pub const fn saved(&self) -> Option<&SavedProfile> {
        self.saved.as_ref()
    }
    pub fn ancestry(&self, unit: UnitId) -> &[AncestryLink] {
        self.links.get(&unit).map_or(&[], Vec::as_slice)
    }
    pub const fn consumed(&self) -> &BTreeSet<Cursor> {
        &self.consumed
    }
    pub(super) fn restored(&self, application: ApplicationId) -> bool {
        self.restored_reclassifications.contains(&application)
    }

    pub fn actual(&self, profile: &UnitProfile) -> Result<CoreProfile, ClimbError> {
        let mut observed = PerCore::default();
        for (core, offset) in profile.expand(&self.units)?.iter() {
            observed.set(core, offset.observed());
        }
        Ok(CoreProfile::new(observed)?)
    }

    /// Refinement changes identities, not physical depth or the number of real applications.
    pub fn refine(&mut self, units: Units, profile: UnitProfile) -> Result<(), ClimbError> {
        if self.current.expand(&self.units)? != profile.expand(&units)? {
            return Err(ClimbError::Refinement);
        }
        let mut links = BTreeMap::new();
        let mut consumed = BTreeSet::new();
        for unit in units.iter() {
            let source = unit.parent.ok_or(ClimbError::Refinement)?;
            if self.units.get(source).is_none() {
                return Err(ClimbError::Refinement);
            }
            links.insert(unit.id, self.ancestry(source).to_vec());
            for cursor in &self.consumed {
                if cursor.unit == source {
                    consumed.insert(Cursor {
                        unit: unit.id,
                        transition: cursor.transition,
                    });
                }
            }
        }
        self.finishes = self.finishes.inherit(&units);
        self.units = units;
        self.current = profile;
        self.links = links;
        self.consumed = consumed;
        Ok(())
    }

    /// A verified reset changes the actual vector without inventing a learning step.
    pub fn observe_application(
        &mut self,
        application: ApplicationId,
        profile: UnitProfile,
    ) -> Result<(), ClimbError> {
        if self.latest.is_some_and(|latest| application <= latest) {
            return Err(ClimbError::ApplicationOrder);
        }
        profile.expand(&self.units)?;
        self.current = profile;
        self.latest = Some(application);
        Ok(())
    }

    /// Call only after a complete successful apply/readback transaction, never for a plan.
    pub fn apply(
        &mut self,
        application: ApplicationId,
        at: Timestamp,
        decision: &ClimbDecision,
    ) -> Result<(), ClimbError> {
        if self.latest.is_some_and(|latest| application <= latest) {
            return Err(ClimbError::ApplicationOrder);
        }
        if decision.before != self.current {
            return Err(ClimbError::StaleDecision);
        }
        let target = decision.target().ok_or(ClimbError::HoldApplication)?;
        target.expand(&self.units)?;
        match &decision.action {
            ClimbAction::FirmwareZero { saved, .. } => {
                self.saved = (!saved.profile.is_zero()).then(|| saved.clone());
            }
            ClimbAction::Promote(_) => self.push_step(application, at, target, StepKind::Ordinary),
            ClimbAction::Restore(proposal) => {
                let kind = match proposal.kind {
                    RestorationKind::Firmware => {
                        self.saved = None;
                        StepKind::FirmwareRestoration
                    }
                    RestorationKind::Reclassified { retreat } => {
                        self.restored_reclassifications.insert(retreat);
                        StepKind::ReclassificationRestoration
                    }
                };
                self.push_step(application, at, target, kind);
            }
            ClimbAction::Retreat(retreat) => {
                self.consumed.extend(retreat.cursors.iter().copied());
                for &unit in &retreat.stopped {
                    let depth = target.get(unit).ok_or(UnitProfileError::Missing(unit))?;
                    let reason = retreat.stop_reason(unit, self);
                    self.finishes.stop(
                        unit,
                        FinishWitness {
                            reason: FinishReason::Stopped(reason),
                            at,
                            profile: target.clone(),
                            last_clean: depth,
                            ceiling: decision.ceilings.get(&unit).cloned(),
                        },
                    );
                }
                self.retreats.push(RetreatRecord {
                    application,
                    units: self.units.clone(),
                    restorable: retreat.adverse_units.is_empty(),
                    before: self.current.clone(),
                    after: target.clone(),
                    events: retreat.events.clone(),
                    cursors: retreat.cursors.clone(),
                    wanted: retreat.wanted.clone(),
                });
            }
            ClimbAction::Hold { .. } => return Err(ClimbError::HoldApplication),
        }
        self.current = target.clone();
        self.latest = Some(application);
        self.finishes
            .observe(at, &self.current, &decision.ceilings, false);
        Ok(())
    }

    fn push_step(
        &mut self,
        application: ApplicationId,
        at: Timestamp,
        target: &UnitProfile,
        kind: StepKind,
    ) {
        let changed: Vec<_> = target
            .iter()
            .filter(|&(unit, offset)| self.current.get(unit) != Some(offset))
            .map(|(unit, _)| unit)
            .collect();
        for &unit in &changed {
            if let (Some(before), Some(after)) = (self.current.get(unit), target.get(unit)) {
                self.links.entry(unit).or_default().push(AncestryLink {
                    transition: TransitionId {
                        application,
                        source: unit,
                    },
                    before,
                    after,
                });
            }
        }
        self.steps.push(LearningStep {
            application,
            at,
            before: SavedProfile {
                units: self.units.clone(),
                profile: self.current.clone(),
            },
            after: SavedProfile {
                units: self.units.clone(),
                profile: target.clone(),
            },
            changed,
            kind,
        });
    }

    /// The newest real transition still available to backstep at `now`, after the supplied
    /// per-unit cursors have consumed earlier retreats. Reconstruction uses the same ancestry
    /// selection as current safety decisions.
    pub fn newest(
        &self,
        unit: UnitId,
        now: TargetOffset,
        consumed: &BTreeSet<Cursor>,
    ) -> Option<AncestryLink> {
        self.ancestry(unit).iter().rev().copied().find(|link| {
            link.before > now
                && !consumed.contains(&Cursor {
                    unit,
                    transition: link.transition,
                })
        })
    }

    pub(super) fn firmware_step(&self, application: ApplicationId) -> bool {
        self.steps.iter().any(|step| {
            step.application == application && step.kind == StepKind::FirmwareRestoration
        })
    }
}

impl super::Retreat {
    fn stop_reason(&self, unit: UnitId, history: &ClimbHistory) -> StopReason {
        if let Some(event) = self.events.first() {
            let application = self
                .cursors
                .iter()
                .find(|cursor| cursor.unit == unit)
                .map(|cursor| cursor.transition.application);
            if let Some(application) = application.filter(|&id| history.firmware_step(id)) {
                StopReason::RestorationBackstep {
                    event: event.clone(),
                    application,
                }
            } else {
                StopReason::FirstBackstep {
                    event: event.clone(),
                    application,
                }
            }
        } else if self.adverse_units.contains(&unit) {
            StopReason::AdverseCeiling
        } else {
            StopReason::Veto
        }
    }
}
