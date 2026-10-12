//! What a simulated machine keeps when the process that drives it exits, as real hardware keeps its clock, its
//! kernel logs and the reset it is about to report.

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use togi_machine::{
    BankType, BiosContext, BootId, CoreId, CpuId, Mce, ResetKind, ResetReason, TargetOffset,
};

use crate::runtime::PendingMce;
use crate::samples::LastTrial;

/// The state of a [`Simulator`](crate::Simulator) that a later process continues from with
/// [`Simulator::restore`](crate::Simulator::restore): its seed, clocks, boots with their reset reasons and kernel
/// logs, the machine checks and reset waiting for the next boot, its registers, BIOS context and the last trial's
/// samples. Draws are keyed by seed and typed parts, so no generator state is kept. Armed faults, scripts added
/// with [`Simulator::script_trial`](crate::Simulator::script_trial), recorded violations and the samples
/// directory are not kept.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Persisted {
    pub(crate) seed: u64,
    pub(crate) epoch: Timestamp,
    pub(crate) clock: Duration,
    pub(crate) boot_at: Duration,
    pub(crate) wall_offset: SignedDuration,
    pub(crate) boot_number: u32,
    pub(crate) boots: Vec<PersistedBoot>,
    pub(crate) regs: Vec<TargetOffset>,
    pub(crate) crashed: bool,
    pub(crate) queued: Vec<PendingMce>,
    pub(crate) next_reset: Option<ResetKind>,
    pub(crate) bios: BiosContext,
    pub(crate) samples: LastTrial,
}

/// A machine that cannot continue from a [`Persisted`] state.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RestoreError {
    #[error("the persisted machine has {persisted} cores, the machine file {model}")]
    Cores { persisted: usize, model: usize },
    #[error("the persisted machine has no boot")]
    NoBoot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedBoot {
    pub id: BootId,
    pub reason: PersistedReason,
    pub mces: Vec<PersistedMce>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedReason {
    pub kind: Option<ResetKind>,
    pub raw: Vec<String>,
    pub supported: bool,
}

impl From<&ResetReason> for PersistedReason {
    fn from(reason: &ResetReason) -> Self {
        Self {
            kind: reason.kind,
            raw: reason.raw.clone(),
            supported: reason.supported,
        }
    }
}

impl From<PersistedReason> for ResetReason {
    fn from(reason: PersistedReason) -> Self {
        Self {
            kind: reason.kind,
            raw: reason.raw,
            supported: reason.supported,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedMce {
    pub cpu: CpuId,
    pub core: Option<CoreId>,
    pub bank: u16,
    pub bank_type: BankType,
    pub corrected: bool,
    pub time: Timestamp,
    pub monotonic: Duration,
    pub lines: Vec<String>,
}

impl From<&Mce> for PersistedMce {
    fn from(mce: &Mce) -> Self {
        Self {
            cpu: mce.cpu,
            core: mce.core,
            bank: mce.bank,
            bank_type: mce.bank_type,
            corrected: mce.corrected,
            time: mce.time,
            monotonic: mce.monotonic,
            lines: mce.lines.clone(),
        }
    }
}

impl From<PersistedMce> for Mce {
    fn from(mce: PersistedMce) -> Self {
        Self {
            cpu: mce.cpu,
            core: mce.core,
            bank: mce.bank,
            bank_type: mce.bank_type,
            corrected: mce.corrected,
            time: mce.time,
            monotonic: mce.monotonic,
            lines: mce.lines,
        }
    }
}
