//! R7 request order and attribution, self-sufficiency evidence, voltage-targeted backoff and
//! escalation to a located hunt, as pure values the tuner state folds into.

pub(crate) mod escalation;
pub(crate) mod failed;
pub(crate) mod measurements;
pub(crate) mod order;
pub(crate) mod probe;
pub(crate) mod query;
pub(crate) mod reason;
pub(crate) mod respond;
pub(crate) mod state;
pub(crate) mod status;
pub(crate) mod target;

use togi_machine::{CoreId, TargetOffset};

use crate::Profile;

/// A profile's offset for a core of its topology. A core it does not hold reads as zero.
pub(crate) trait Applied {
    fn at(&self, core: CoreId) -> TargetOffset;
}

impl Applied for Profile {
    fn at(&self, core: CoreId) -> TargetOffset {
        self.get(core).unwrap_or(TargetOffset::ZERO)
    }
}

#[cfg(test)]
mod tests;
