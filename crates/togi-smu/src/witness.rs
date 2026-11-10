use togi_machine::{CoreId, ObservedOffset, PerCore, TargetOffset};

/// A complete sixteen-core, distinct, nonzero offset witness.
///
/// This module only supplies targets and validates readback. A production caller must use
/// the session's journaled write transaction, then restore and verify stock. Only the
/// opt-in hardware tests may perform the unjournaled maintainer round trip.
#[derive(Debug)]
pub struct OffsetWitness(PerCore<TargetOffset>);

#[derive(Debug, thiserror::Error)]
pub enum WitnessError {
    #[error("offset witness requires sixteen cores, got {count}")]
    CoreCount { count: usize },
    #[error("offset witness requires a nonzero target for core {core}")]
    Zero { core: CoreId },
    #[error("offset witness repeats target {target:?} at core {core}")]
    Duplicate { core: CoreId, target: TargetOffset },
    #[error("offset witness readback differs at core {core}: wanted {target:?}, got {observed:?}")]
    Readback {
        core: CoreId,
        target: TargetOffset,
        observed: Option<ObservedOffset>,
    },
}

impl OffsetWitness {
    pub fn new(targets: PerCore<TargetOffset>) -> Result<Self, WitnessError> {
        if targets.len() != 16 {
            return Err(WitnessError::CoreCount {
                count: targets.len(),
            });
        }
        let mut seen = 0_u64;
        for (core, target) in targets.iter() {
            if target == TargetOffset::ZERO {
                return Err(WitnessError::Zero { core });
            }
            let bit = 1_u64 << target.get().unsigned_abs();
            if seen & bit != 0 {
                return Err(WitnessError::Duplicate { core, target });
            }
            seen |= bit;
        }
        Ok(Self(targets))
    }

    /// Maps cores 0–15 to -1 through -16; incomplete topologies are refused.
    pub fn fixture(cores: impl IntoIterator<Item = CoreId>) -> Result<Self, WitnessError> {
        Self::new(
            cores
                .into_iter()
                .map(|core| (core, TargetOffset::clamped(-1 - i64::from(core.get()))))
                .collect(),
        )
    }

    pub fn targets(&self) -> &PerCore<TargetOffset> {
        &self.0
    }

    /// Checks the complete vector after all writes, so aliased slots cannot pass by
    /// reading each value immediately after writing it.
    pub fn verify(&self, observed: &PerCore<ObservedOffset>) -> Result<(), WitnessError> {
        for (core, target) in self.0.iter() {
            let read = observed.get(core);
            if read != Some(target.observed()) {
                return Err(WitnessError::Readback {
                    core,
                    target,
                    observed: read,
                });
            }
        }
        Ok(())
    }
}
