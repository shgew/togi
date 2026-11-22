//! Read the frozen gzip trial-fact extracts used by the fitted simulator's replay oracle.

mod file;
mod record;
mod replay;

pub use file::{ReadError, decode};
pub use record::{
    BuildStamp, Class, Decisive, InvalidProfile, Phase, Profile, TrialClass, TrialFact,
};
pub use replay::{Extracts, ExtractsError, replay_facts};
