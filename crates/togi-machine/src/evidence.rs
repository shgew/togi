//! What the kernel log of a crashed boot and of its immediate successor says about how the boot ended.
//! Raw log text is parsed once at the kernel reader; everything past it carries these types.

use serde::{Deserialize, Serialize};

use crate::object_only;

/// The most distinct faults one tail keeps; later distinct faults are dropped, the first ones stay.
pub const MAX_FAULTS: usize = 16;

/// A graphics kernel driver whose reset or ring timeout togi recognizes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphicsDriver {
    Amdgpu,
    I915,
    Xe,
}

impl GraphicsDriver {
    /// The kernel's name for the driver.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Amdgpu => "amdgpu",
            Self::I915 => "i915",
            Self::Xe => "xe",
        }
    }
}

object_only! {
    /// One kind of error the kernel logged near the end of a boot: an uncorrected or fatal machine check, a
    /// graphics ring, job or engine timeout, a graphics reset the driver started, an NVIDIA `Xid` report, a
    /// kernel oops, BUG or panic, or a task blocked past the hung-task timeout.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum KernelFault {
        MachineCheck {},
        GraphicsTimeout { driver: GraphicsDriver },
        GraphicsReset { driver: GraphicsDriver },
        NvidiaXid { code: u32 },
        Oops {},
        HungTask {},
    }
}

impl KernelFault {
    /// Whether the fault is the graphics hardware or its driver failing, which the CPU did not cause.
    pub const fn is_graphics(self) -> bool {
        match self {
            Self::GraphicsTimeout { .. } | Self::GraphicsReset { .. } | Self::NvidiaXid { .. } => {
                true
            }
            Self::MachineCheck {} | Self::Oops {} | Self::HungTask {} => false,
        }
    }
}

object_only! {
    /// The end of a boot's kernel log: `Missing` when the boot has no kernel log and `Unreadable` when reading
    /// it failed (both leave the boot's end unknown), `Quiet` when the tail was read and holds no recognized
    /// error, and `Faults` otherwise.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum KernelTail {
        Missing {},
        Unreadable {},
        Quiet {},
        Faults {
            /// Distinct errors in order of last appearance, so the final one is the last error logged;
            /// `MAX_FAULTS` at most.
            faults: Vec<KernelFault>,
            /// Whether the log held entries after the last error.
            logging_continued: bool,
        },
    }
}

impl KernelTail {
    /// The recognized errors; empty unless the tail was read and holds some.
    pub fn faults(&self) -> &[KernelFault] {
        match self {
            Self::Faults { faults, .. } => faults,
            Self::Missing {} | Self::Unreadable {} | Self::Quiet {} => &[],
        }
    }

    /// The last recognized error the log held, if the tail was read and holds any.
    pub fn last_fault(&self) -> Option<KernelFault> {
        self.faults().last().copied()
    }

    /// Whether the tail was read, so that the absence of an error means something.
    pub const fn was_read(&self) -> bool {
        match self {
            Self::Quiet {} | Self::Faults { .. } => true,
            Self::Missing {} | Self::Unreadable {} => false,
        }
    }
}

object_only! {
    /// The machine-check events one boot's kernel log holds; `Unknown` when the log could not be read, which is
    /// not zero.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum MceCount {
        Unknown {},
        Read { corrected: u32, uncorrected: u32 },
    }
}

impl MceCount {
    /// Whether the log was read and holds an uncorrected machine check.
    pub const fn has_uncorrected(self) -> bool {
        match self {
            Self::Read { uncorrected, .. } => uncorrected > 0,
            Self::Unknown {} => false,
        }
    }
}

/// The kind of sleep a boot's kernel log shows it entering last.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SleepMode {
    /// Suspend to idle or to RAM.
    Suspend,
    Hibernate,
}

impl SleepMode {
    /// The kernel event the log ends at, for messages.
    pub const fn entry_words(self) -> &'static str {
        match self {
            Self::Suspend => "a suspend entry",
            Self::Hibernate => "a hibernation entry",
        }
    }
}

object_only! {
    /// What the kernel logs of a crashed boot and of its immediate successor say about how it ended.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct EndEvidence {
        /// Machine checks in the crashed boot's own log.
        pub crashed_mces: MceCount,
        /// Machine checks in the immediate successor's log, which carries those the crash prevented logging.
        pub successor_mces: MceCount,
        pub tail: KernelTail,
        /// The sleep the crashed boot's log ends in: its last suspend or hibernation entry has no exit after
        /// it. Absent when the log shows no such entry, could not be read, or an earlier record predates it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub asleep: Option<SleepMode>,
    }
}

impl EndEvidence {
    /// Nothing could be read: no counts and a missing tail.
    pub const fn unknown() -> Self {
        Self {
            crashed_mces: MceCount::Unknown {},
            successor_mces: MceCount::Unknown {},
            tail: KernelTail::Missing {},
            asleep: None,
        }
    }

    /// Whether either log holds an uncorrected machine check.
    pub const fn has_uncorrected_mce(&self) -> bool {
        self.crashed_mces.has_uncorrected() || self.successor_mces.has_uncorrected()
    }
}
