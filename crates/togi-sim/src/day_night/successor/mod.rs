//! Separately registered, full-physical-core truth for successor experiments.

mod catalog;
mod exposure;
mod reference;

use super::{DailyBlock, Scenario as OriginalScenario, TrialClass, TruthError};
use togi_machine::{CoreId, CoreInfo, PerCore, TargetOffset};

pub use catalog::SCENARIO_IDS;
pub use exposure::{
    Exposure, Failure, FailureCause, InteractionExposure, InteractionId, SliceSeconds, TruthState,
};
pub use reference::{
    Admissibility, Certificate, DepthReference, InteractionRate, Reference, ReferenceCalendar,
    UnitGrouping,
};

pub const LAMBDA: f64 = 1.0 / 3000.0;
pub const OPERATIONAL_DEPTH: u8 = 45;
/// Absolute tolerance in failures per active machine-hour, not extra risk permission.
pub const NUMERICAL_TOLERANCE: f64 = 1e-14;

/// Checked nonnegative physical-core depth in the runtime write range.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Depth(u8);
impl Depth {
    pub fn new(value: u8) -> Result<Self, TruthError> {
        if value <= 50 {
            Ok(Self(value))
        } else {
            Err(TruthError::Invalid("physical-core depth"))
        }
    }
    pub const fn get(self) -> u8 {
        self.0
    }
    pub fn offset(self) -> Result<TargetOffset, TruthError> {
        super::offset_for_depth(usize::from(self.0))
    }
}

/// Complete dense physical-core profile; construction refuses holes and extra cores.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Profile {
    depths: PerCore<Depth>,
}
impl Profile {
    pub fn new(depths: PerCore<Depth>, topology: &[CoreInfo]) -> Result<Self, TruthError> {
        if depths.len() != topology.len()
            || topology.iter().any(|info| depths.get(info.core).is_none())
        {
            return Err(TruthError::Invalid("full physical-core profile"));
        }
        Ok(Self { depths })
    }
    pub fn depths(&self) -> &PerCore<Depth> {
        &self.depths
    }
    pub fn uniform(topology: &[CoreInfo], depth: Depth) -> Self {
        Self {
            depths: topology.iter().map(|info| (info.core, depth)).collect(),
        }
    }
    pub fn in_envelope(&self) -> bool {
        self.depths
            .iter()
            .all(|(_, depth)| depth.get() <= OPERATIONAL_DEPTH)
    }
    pub fn mean_depth(&self) -> f64 {
        self.depths
            .iter()
            .map(|(_, depth)| f64::from(depth.get()))
            .sum::<f64>()
            / number(self.depths.len())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EpochKind {
    Initial,
    Moving,
    Firmware,
}
/// Days are one-based; this epoch applies before the indicated day's active use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TruthEpoch {
    pub index: u32,
    pub first_day: u32,
    pub kind: EpochKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fixture {
    Legacy,
    Heterogeneous,
    Hidden,
    Delayed,
    Combined,
    Interactions,
    Refinement,
}

#[derive(Clone, Debug)]
pub struct Scenario {
    id: &'static str,
    original: OriginalScenario,
    fixture: Fixture,
    weak: Vec<CoreId>,
    daily: Vec<DailyBlock>,
    nights: Vec<TrialClass>,
    interactions: Vec<(CoreId, CoreId)>,
}

impl Scenario {
    pub fn builtin(id: &str, seed: u64) -> Result<Self, TruthError> {
        catalog::builtin(id, seed)
    }
    pub const fn id(&self) -> &'static str {
        self.id
    }
    pub fn topology(&self) -> &[CoreInfo] {
        self.original.topology()
    }
    pub fn daily_mix(&self) -> &[DailyBlock] {
        &self.daily
    }
    pub fn night_classes(&self) -> &[TrialClass] {
        &self.nights
    }
    pub fn stock_background_per_hour(&self) -> f64 {
        self.original.stock_background_per_hour()
    }
    pub fn non_cpu(&self) -> Option<super::NonCpuSchedule> {
        self.original.non_cpu()
    }
    pub fn epoch(&self, day: u32) -> Result<TruthEpoch, TruthError> {
        if day == 0 {
            return Err(TruthError::Invalid("one-based calendar day"));
        }
        let index = self.original.cap_epoch(day - 1);
        let (first_day, kind) = match self.original.family {
            super::Family::Moving if index > 0 => (index * 45 + 1, EpochKind::Moving),
            super::Family::Firmware if index > 0 => (181, EpochKind::Firmware),
            super::Family::Ordinary
            | super::Family::WriteLimitSafe
            | super::Family::FlatterDangerous
            | super::Family::Delayed
            | super::Family::Thermal
            | super::Family::Hidden
            | super::Family::Partial
            | super::Family::SingleCore
            | super::Family::Joint
            | super::Family::R8Nonbinding
            | super::Family::Interaction
            | super::Family::Moving
            | super::Family::Firmware => (1, EpochKind::Initial),
        };
        Ok(TruthEpoch {
            index,
            first_day,
            kind,
        })
    }
    /// Missing coverage is an exact class, never a guessed safe profile.
    pub fn missing_night_classes(&self) -> Vec<&TrialClass> {
        self.daily
            .iter()
            .map(|block| &block.class)
            .filter(|class| !self.nights.contains(class))
            .collect()
    }
    /// Registered coarse result and night-discoverable candidates, not a restricted objective.
    pub fn refinement(&self) -> Result<Option<Refinement>, TruthError> {
        if self.fixture != Fixture::Refinement {
            return Ok(None);
        }
        let inherited = Profile::uniform(self.topology(), Depth::new(35)?);
        let mut candidates = Vec::new();
        let mut ceilings = PerCore::default();
        let mut night_edges = PerCore::default();
        for info in self.topology() {
            let ceiling = if self.weak.contains(&info.core) {
                36
            } else {
                40
            };
            ceilings.set(info.core, Depth::new(ceiling)?);
            night_edges.set(info.core, Depth::new(ceiling + 5)?);
            for depth in 36..=ceiling {
                let mut profile = inherited.clone();
                profile.depths.set(info.core, Depth::new(depth)?);
                candidates.push((info.core, profile));
            }
        }
        Ok(Some(Refinement {
            inherited,
            night_edges,
            ceilings,
            candidates,
        }))
    }
    pub(super) fn check_profile(&self, profile: &Profile) -> Result<(), TruthError> {
        if profile.depths.len() != self.topology().len()
            || self
                .topology()
                .iter()
                .any(|info| profile.depths.get(info.core).is_none())
        {
            return Err(TruthError::Invalid("profile topology"));
        }
        Ok(())
    }
    pub(super) fn core_rate(
        &self,
        class: &TrialClass,
        core: CoreId,
        depth: Depth,
        day: u32,
    ) -> Result<f64, TruthError> {
        let info = self
            .topology()
            .iter()
            .find(|info| info.core == core)
            .ok_or(TruthError::Invalid("core topology"))?;
        if !class.loaded().contains(core) {
            return Ok(0.0);
        }
        if self.fixture == Fixture::Legacy {
            let count = class
                .cores
                .iter()
                .filter(|id| {
                    self.topology()
                        .iter()
                        .any(|other| other.core == **id && other.ccd == info.ccd)
                })
                .count();
            return Ok(self
                .original
                .curve(class, info.ccd.index(), day - 1)?
                .excess_per_hour(depth.offset()?)
                / number(count));
        }
        let weak = self.weak.contains(&core);
        let hidden = matches!(self.fixture, Fixture::Hidden | Fixture::Combined);
        let binding =
            !hidden || class.workload() == super::scenarios::workload(togi_machine::Regime::R2, 2)?;
        let ceiling = match self.fixture {
            Fixture::Heterogeneous => {
                if weak {
                    20
                } else {
                    40
                }
            }
            Fixture::Hidden | Fixture::Delayed | Fixture::Combined => {
                if weak && binding {
                    20
                } else {
                    40
                }
            }
            Fixture::Interactions => 40,
            // First failing counts 41/45 give U+5 ceilings 36/40.
            Fixture::Refinement => {
                if weak {
                    40
                } else {
                    44
                }
            }
            Fixture::Legacy => return Err(TruthError::Invalid("fixture curve")),
        };
        Ok(if depth.get() <= ceiling {
            0.0
        } else {
            32.0 * LAMBDA * 2.0_f64.powi(i32::from(depth.get() - ceiling - 1))
        })
    }
    pub(super) fn joint_pairs(&self) -> &[(CoreId, CoreId)] {
        &self.interactions
    }
    pub(super) fn joint_root(&self, depth: Depth) -> Result<f64, TruthError> {
        if self.fixture == Fixture::Legacy {
            Ok(self
                .original
                .interaction_curve()?
                .ok_or(TruthError::Invalid("interaction curve"))?
                .excess_per_hour(depth.offset()?)
                .sqrt())
        } else {
            Ok(if depth.get() > 20 {
                (4.0 * LAMBDA).sqrt()
            } else {
                0.0
            })
        }
    }
}

#[derive(Clone, Debug)]
pub struct Refinement {
    pub inherited: Profile,
    pub night_edges: PerCore<Depth>,
    pub ceilings: PerCore<Depth>,
    pub candidates: Vec<(CoreId, Profile)>,
}

#[expect(
    clippy::cast_precision_loss,
    reason = "physical topology counts are bounded to sixteen"
)]
pub(super) fn number(value: usize) -> f64 {
    value as f64
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
