//! The simulator suite check: `tools/bench/suite.json` expanded into runs, the runs partitioned into shards, and the
//! aggregate that proves every run of the suite was executed once and ended clean.

mod coverage;
mod run;
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
