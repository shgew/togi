use serde::{Deserialize, Deserializer, Serialize};

pub const PER_CORE_MAX: usize = 16;
pub const MIN_OFFSET: i64 = -50;
pub const MAX_OFFSET: i64 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("core {0} outside 0–15")]
pub struct InvalidCore(pub i64);

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct CoreId(u8);

impl CoreId {
    pub fn new(value: i64) -> Result<Self, InvalidCore> {
        match u8::try_from(value) {
            Ok(value) if usize::from(value) < PER_CORE_MAX => Ok(Self(value)),
            _ => Err(InvalidCore(value)),
        }
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
    pub const fn get(self) -> u8 {
        self.0
    }
    pub(crate) const fn from_index(value: u8) -> Self {
        Self(value)
    }
}

impl<'de> Deserialize<'de> for CoreId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for CoreId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct CcdId(pub i64);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct CpuId(pub i64);

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ObservedOffset(pub i64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("offset {0} outside -50–0")]
pub struct InvalidTarget(pub i64);

/// A target cannot bypass the checked or clamping constructor.
///
/// ```compile_fail
/// let unsafe_target = togi_machine::TargetOffset(10);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TargetOffset(i64);

impl TargetOffset {
    pub const ZERO: Self = Self(0);

    pub fn new(value: i64) -> Result<Self, InvalidTarget> {
        if (MIN_OFFSET..=MAX_OFFSET).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidTarget(value))
        }
    }

    pub fn clamped(value: i64) -> Self {
        Self(value.clamp(MIN_OFFSET, MAX_OFFSET))
    }
    pub const fn get(self) -> i64 {
        self.0
    }
    pub const fn observed(self) -> ObservedOffset {
        ObservedOffset(self.0)
    }
}

impl<'de> Deserialize<'de> for TargetOffset {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(i64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NanoDuration(pub i64);
