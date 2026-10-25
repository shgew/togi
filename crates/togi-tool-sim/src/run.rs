use std::fmt;
use std::num::NonZeroU32;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::Split;

/// How long the simulated operator lets a run's night last when its scenario does not say.
pub const DEFAULT_NIGHT_HOURS: NonZeroU32 = match NonZeroU32::new(8) {
    Some(hours) => hours,
    None => NonZeroU32::MIN,
};

/// How a run's night is expected to end. A run passes only when its night ends this way, its replay is identical and
/// its audit is clean.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedEnd {
    /// The simulated operator ends the night after its hours.
    #[default]
    Operator,
    /// The night ends at a dead end the run's machine produces: a conclusive failure at CO 0, a runtime safety stop
    /// or every backend exhausted.
    DeadEnd,
    /// Either the operator's end or a dead end for a conclusive failure at CO 0 whose cited exposure ran every core
    /// at zero: the machine's model has a hazard at stock, so whether a seed fails there is the seed's draw.
    OperatorOrStock,
}

impl fmt::Display for ExpectedEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Operator => "an operator end",
            Self::DeadEnd => "a dead end",
            Self::OperatorOrStock => "an operator end or a stock dead end",
        })
    }
}

/// The stable name of one run of the suite: `<scenario>/<split>-<seed>`. Scenario names are validated as one
/// filesystem-safe component, so the id is a two-component relative path usable as the run's state directory and
/// is the key coverage is checked by.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    pub(crate) fn new(scenario: &str, split: Split, seed: u64) -> Self {
        Self(format!("{scenario}/{split}-{seed}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Run {
    pub id: RunId,
    pub scenario: String,
    pub split: Split,
    pub seed: u64,
    pub machine: Option<PathBuf>,
    pub replay: bool,
    pub smoke: bool,
    /// How many hours the simulated operator lets the run's night last.
    pub night_hours: NonZeroU32,
    pub expected_end: ExpectedEnd,
}
