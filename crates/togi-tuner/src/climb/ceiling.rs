use std::collections::BTreeMap;

use togi_machine::TargetOffset;

use crate::unit::UnitId;

/// Original evidence identity, supplied by the evidence layer rather than a journal type.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EvidenceId(u64);

impl EvidenceId {
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Current-context isolated evidence. A completed descent is not a failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EdgeState {
    Unknown,
    Observed {
        offset: TargetOffset,
        witness: EvidenceId,
    },
    Censored {
        witnesses: Vec<EvidenceId>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ceiling {
    pub edge: EdgeState,
    pub c: Option<TargetOffset>,
}

impl Ceiling {
    pub fn new(edge: EdgeState) -> Self {
        let c = match &edge {
            EdgeState::Unknown => None,
            EdgeState::Observed { offset, .. } => Some(TargetOffset::clamped(offset.get() + 5)),
            EdgeState::Censored { .. } => Some(TargetOffset::clamped(-45)),
        };
        Self { edge, c }
    }
}

pub type Ceilings = BTreeMap<UnitId, Ceiling>;
