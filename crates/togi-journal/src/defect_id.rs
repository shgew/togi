//! A known defect's identity and a build's fixes stamp. They live here, beside the events that record them, so
//! the defect registry and every journal reader share them without a dependency cycle.

use std::fmt;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

/// A known defect's number: positive, unique in the registry and recorded as the plain number.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DefectId(NonZeroU32);

impl DefectId {
    /// `None` for 0, which no defect has.
    pub const fn new(id: u32) -> Option<Self> {
        match NonZeroU32::new(id) {
            Some(id) => Some(Self(id)),
            None => None,
        }
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for DefectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The number of the last defect a build fixed: every defect up to it is fixed in that build. Recorded as the
/// plain number, 0 when the build fixed none.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FixStamp(u32);

impl FixStamp {
    /// The stamp of a build that fixed no defect, and of a build stamp that omits the field.
    pub const fn none() -> Self {
        Self(0)
    }

    /// The stamp of a build whose last fixed defect is `id`.
    pub const fn through(id: DefectId) -> Self {
        Self(id.get())
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// Whether a build with this stamp has defect `id` fixed.
    pub const fn fixes(self, id: DefectId) -> bool {
        self.0 >= id.get()
    }
}

impl fmt::Display for FixStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
