//! Versioned scripted sessions for exact mechanics comparisons and the fitted research suite's
//! manifest and shard accounting.

mod coverage;
mod run;
pub mod scripted;
mod shard;
mod split;
mod suite;

pub use coverage::{
    CoverageError, ExpectedShard, ManifestError, Outcome, Problem, RunResult, ShardManifest, check,
    encode_manifest, parse_manifest,
};
pub use run::{DEFAULT_NIGHT_HOURS, ExpectedEnd, Run, RunId};
pub use shard::{ShardError, partition, shard};
pub use split::Split;
pub use suite::{Suite, SuiteError};
