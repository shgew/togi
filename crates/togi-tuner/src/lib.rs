//! The tuning rules: the core topology, topology-complete profiles, the failure points and
//! combinations that bound them with the safe optimum they allow, and crash classification.

mod constraints;
mod crash;
mod depth;
mod optimum;
mod profile;
mod topology;

#[cfg(test)]
mod tests;

pub use constraints::{
    Combination, CombinationId, ConstraintError, Constraints, FailurePoint, Insertion, Limit,
    Reached,
};
pub use crash::{CrashFacts, CrashKind, classify_crash, recorded_kind};
pub use depth::Depth;
pub use profile::{Profile, ProfileError};
pub use topology::{Ranking, RankingError, Topology, TopologyError};
