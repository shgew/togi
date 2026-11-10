//! CPU-frequency policy access, independent of trial lifetime and persistence.

use std::io;

use serde::{Deserialize, Serialize};

use crate::{BootId, CpuId, object_only};

/// The kernel's numeric `policyN` identity, never a caller-supplied path.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FrequencyPolicyId(u32);

impl FrequencyPolicyId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A policy's membership and current limits, all frequencies in kHz.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrequencyPolicy {
    pub id: FrequencyPolicyId,
    pub related_cpus: Vec<CpuId>,
    pub cpuinfo_min_khz: u32,
    pub scaling_min_khz: u32,
    pub scaling_max_khz: u32,
    /// `None` for drivers with a continuous range rather than a frequency table.
    pub available_khz: Option<Vec<u32>>,
}

/// Whether the frequency write applies a trial cap or restores its original limit.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrequencyAction {
    Cap,
    Restore,
}

object_only! {
    /// Audit evidence acknowledged durably before a frequency-limit write.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FrequencyIntent {
        pub policy: FrequencyPolicyId,
        pub related_cpus: Vec<CpuId>,
        pub original_khz: u32,
        pub cap_khz: u32,
        pub action: FrequencyAction,
    }
}

object_only! {
    /// The observed frequency limit after the write described by `intent`.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FrequencyReadback {
        pub intent: FrequencyIntent,
        pub observed_khz: u32,
    }
}

object_only! {
    /// A pending recovery record discarded without writing a frequency limit.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FrequencyDiscarded {
        pub boot: BootId,
        pub reason: String,
    }
}

/// Audit-only frequency evidence; the pending file alone owns recovery intent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FrequencyEvent {
    Intent(FrequencyIntent),
    Readback(FrequencyReadback),
    Discarded(FrequencyDiscarded),
}

/// A successful return acknowledges a durable append, never merely queued evidence.
pub trait FrequencyJournal {
    fn record(&mut self, event: FrequencyEvent) -> io::Result<()>;
}

/// Only the adapter touches cpufreq sysfs. The trial owns durable intent and readback checks.
pub trait CpuFrequency: Send + Sync {
    fn boot_id(&self) -> io::Result<BootId>;
    fn policies(&self) -> io::Result<Vec<FrequencyPolicy>>;
    fn scaling_max_khz(&self, policy: FrequencyPolicyId) -> io::Result<u32>;
    fn set_scaling_max_khz(&self, policy: FrequencyPolicyId, khz: u32) -> io::Result<()>;
}
