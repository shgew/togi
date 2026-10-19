use serde::{Deserialize, Serialize};

pub const PER_CORE_MAX: usize = 16;
pub const MIN_OFFSET: i64 = -50;
pub const MAX_OFFSET: i64 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("core {0} outside 0–15")]
pub struct InvalidCore(pub i128);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("ccd {0} outside 0–15")]
pub struct InvalidCcd(pub i128);

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("cpu {0} outside 0–65535")]
pub struct InvalidCpu(pub i128);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "i64")]
pub struct CoreId(u8);

impl CoreId {
    /// The lowest core: every machine has it.
    pub const FIRST: Self = Self(0);

    pub fn new(value: i64) -> Result<Self, InvalidCore> {
        match u8::try_from(value) {
            Ok(value) if usize::from(value) < PER_CORE_MAX => Ok(Self(value)),
            _ => Err(InvalidCore(i128::from(value))),
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

impl TryFrom<u8> for CoreId {
    type Error = InvalidCore;
    fn try_from(value: u8) -> Result<Self, InvalidCore> {
        Self::new(i64::from(value))
    }
}

impl TryFrom<usize> for CoreId {
    type Error = InvalidCore;
    fn try_from(value: usize) -> Result<Self, InvalidCore> {
        i64::try_from(value)
            .map_err(|_| InvalidCore(value as i128))
            .and_then(Self::new)
    }
}

impl TryFrom<i64> for CoreId {
    type Error = InvalidCore;
    fn try_from(value: i64) -> Result<Self, InvalidCore> {
        Self::new(value)
    }
}

impl std::fmt::Display for CcdId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::fmt::Display for CpuId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::fmt::Display for CoreId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A CCD's dense index on the package: CCDs are numbered from 0 in order, and each holds at least one of at
/// most [`PER_CORE_MAX`] cores.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "i64")]
pub struct CcdId(u8);

impl CcdId {
    pub fn new(value: i64) -> Result<Self, InvalidCcd> {
        match u8::try_from(value) {
            Ok(value) if usize::from(value) < PER_CORE_MAX => Ok(Self(value)),
            _ => Err(InvalidCcd(i128::from(value))),
        }
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for CcdId {
    type Error = InvalidCcd;
    fn try_from(value: u8) -> Result<Self, InvalidCcd> {
        Self::new(i64::from(value))
    }
}

impl TryFrom<i64> for CcdId {
    type Error = InvalidCcd;
    fn try_from(value: i64) -> Result<Self, InvalidCcd> {
        Self::new(value)
    }
}

impl TryFrom<usize> for CcdId {
    type Error = InvalidCcd;
    fn try_from(value: usize) -> Result<Self, InvalidCcd> {
        i64::try_from(value)
            .map_err(|_| InvalidCcd(value as i128))
            .and_then(Self::new)
    }
}

/// A logical CPU's kernel number. The kernel numbers every CPU togi can see below 65536, the width the journal
/// records.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CpuId(u16);

impl CpuId {
    pub fn new(value: i64) -> Result<Self, InvalidCpu> {
        u16::try_from(value)
            .map(Self)
            .map_err(|_| InvalidCpu(i128::from(value)))
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl From<u16> for CpuId {
    fn from(value: u16) -> Self {
        Self(value)
    }
}

impl TryFrom<u32> for CpuId {
    type Error = InvalidCpu;
    fn try_from(value: u32) -> Result<Self, InvalidCpu> {
        Self::new(i64::from(value))
    }
}

impl TryFrom<usize> for CpuId {
    type Error = InvalidCpu;
    fn try_from(value: usize) -> Result<Self, InvalidCpu> {
        i64::try_from(value)
            .map_err(|_| InvalidCpu(value as i128))
            .and_then(Self::new)
    }
}

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
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(try_from = "i64")]
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

impl TryFrom<i64> for TargetOffset {
    type Error = InvalidTarget;
    fn try_from(value: i64) -> Result<Self, InvalidTarget> {
        Self::new(value)
    }
}
