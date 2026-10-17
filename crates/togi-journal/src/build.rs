use std::cmp::Ordering;
use std::fmt;

use serde::Serialize;
use togi_machine::object_only;

pub const SCHEMA: u32 = 1;

object_only! {
    #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Build {
        pub version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub rev: Option<String>,
        pub ruleset: u32,
        pub schema: u32,
        #[serde(default)]
        pub fixes: u32,
        pub evidence: u32,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Resume,
    Append,
    ResetAll,
    Inspect,
    Replay,
    History,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dimension {
    Schema,
    Ruleset,
    Evidence,
}

impl fmt::Display for Dimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Schema => "schema",
            Self::Ruleset => "ruleset",
            Self::Evidence => "evidence epoch",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("the journal's {dimension} is {recorded}; this build's is {current}")]
pub struct Refusal {
    pub dimension: Dimension,
    pub recorded: u32,
    pub current: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    Use,
    Archive,
    Refuse(Refusal),
}

impl fmt::Display for Build {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("togi")?;
        if !self.version.is_empty() {
            write!(f, " {}", self.version)?;
        }
        if let Some(rev) = self.rev.as_deref().filter(|rev| !rev.is_empty()) {
            write!(f, "+{rev}")?;
        }
        Ok(())
    }
}

impl Build {
    pub fn verdict(&self, recorded: &Self, operation: Operation) -> Verdict {
        let dimensions = [
            (Dimension::Schema, recorded.schema, self.schema),
            (Dimension::Ruleset, recorded.ruleset, self.ruleset),
            (Dimension::Evidence, recorded.evidence, self.evidence),
        ];
        let refusal = |(dimension, recorded, current): (Dimension, u32, u32)| {
            Verdict::Refuse(Refusal {
                dimension,
                recorded,
                current,
            })
        };
        let order = |(_, recorded, current): &(Dimension, u32, u32)| recorded.cmp(current);
        let first_newer = dimensions.iter().find(|d| order(d) == Ordering::Greater);
        let first_different = dimensions.iter().find(|d| order(d) != Ordering::Equal);
        let schema = dimensions[0];
        let ruleset = dimensions[1];
        match operation {
            Operation::Resume => match (first_newer, first_different) {
                (Some(newer), _) => refusal(*newer),
                (None, Some(_)) => Verdict::Archive,
                (None, None) => Verdict::Use,
            },
            Operation::Append => first_different.map_or(Verdict::Use, |d| refusal(*d)),
            Operation::ResetAll => match order(&schema) {
                Ordering::Equal => Verdict::Use,
                Ordering::Less | Ordering::Greater => Verdict::Archive,
            },
            Operation::Inspect => match order(&schema) {
                Ordering::Equal => Verdict::Use,
                Ordering::Less | Ordering::Greater => refusal(schema),
            },
            Operation::Replay => {
                if order(&schema) == Ordering::Greater {
                    refusal(schema)
                } else if order(&ruleset) != Ordering::Equal {
                    refusal(ruleset)
                } else {
                    Verdict::Use
                }
            }
            Operation::History => {
                if order(&schema) == Ordering::Greater || recorded.schema == 0 {
                    refusal(schema)
                } else {
                    Verdict::Use
                }
            }
        }
    }
}
