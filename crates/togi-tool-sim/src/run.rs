use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Dev,
    Holdout,
}

impl Split {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Holdout => "holdout",
        }
    }
}

impl fmt::Display for Split {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
}
