use std::num::NonZeroU64;

use togi_machine::{ResetKind, ResetReason};

use crate::parse::digits;

const REASON_PREFIX: &str = "x86/amd: Previous system reset reason [0x";
const VERSION_PREFIX: &str = "Linux version ";
const FIRST_SUPPORTED: (u64, u64) = (6, 16);

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[error("the kernel version {0:?} is not a number")]
pub struct VersionError(String);

fn kind(text: &str) -> ResetKind {
    match text {
        "hardware watchdog timer expired" => ResetKind::Watchdog,
        "an uncorrected error caused a data fabric sync flood event"
        | "a software sync flood event occurred" => ResetKind::SyncFlood,
        "internal CPU shutdown event occurred" => ResetKind::CpuShutdown,
        "power button was pressed for 4 seconds" => ResetKind::PowerButton,
        "thermal pin BP_THERMTRIP_L was tripped" | "internal CPU thermal limit was tripped" => {
            ResetKind::ThermalTrip
        }
        _ => ResetKind::Unknown,
    }
}

const fn severity(kind: ResetKind) -> u8 {
    match kind {
        ResetKind::PowerLoss => 0,
        ResetKind::Unknown => 1,
        ResetKind::PowerButton => 2,
        ResetKind::CpuShutdown => 3,
        ResetKind::SyncFlood => 4,
        ResetKind::Watchdog => 5,
        ResetKind::ThermalTrip => 6,
    }
}

fn reason_text(text: &str) -> Option<&str> {
    let rest = text.strip_prefix(REASON_PREFIX)?;
    let (code, reason) = rest.split_at_checked(8)?;
    let reason = reason.strip_prefix("]: ")?;
    (code.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && !reason.is_empty()
        && !reason.contains('\n'))
    .then_some(reason)
}

fn version(text: &str) -> Result<Option<(u64, u64)>, VersionError> {
    let Some(rest) = text.strip_prefix(VERSION_PREFIX) else {
        return Ok(None);
    };
    let Some((major, rest)) = digits(rest) else {
        return Ok(None);
    };
    let Some((minor, _)) = rest.strip_prefix('.').and_then(digits) else {
        return Ok(None);
    };
    let number = |digits: &str| digits.parse().map_err(|_| VersionError(text.to_owned()));
    Ok(Some((number(major)?, number(minor)?)))
}

/// Folds the kernel messages of a boot into what they say about the reset before it.
#[derive(Default)]
pub(crate) struct ResetScan {
    supported: bool,
    kind: Option<ResetKind>,
    raw: Vec<String>,
}

impl ResetScan {
    pub(crate) fn push(&mut self, text: &str) -> Result<(), VersionError> {
        if let Some(found) = version(text)? {
            self.supported |= found >= FIRST_SUPPORTED;
        }
        if let Some(reason) = reason_text(text) {
            let found = kind(reason);
            if self
                .kind
                .is_none_or(|kind| severity(found) > severity(kind))
            {
                self.kind = Some(found);
            }
            self.raw.push(reason.to_owned());
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> ResetReason {
        ResetReason {
            kind: self.kind,
            raw: self.raw,
            supported: self.supported,
        }
    }
}

/// An entry of the whole journal, whose sequence number counts every entry of its sequence id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Position {
    sequence_id: String,
    sequence: NonZeroU64,
}

impl Position {
    pub(crate) fn new(sequence_id: String, sequence: u64) -> Option<Self> {
        Some(Self {
            sequence_id,
            sequence: NonZeroU64::new(sequence)?,
        })
    }

    /// Whether `next` is the very next entry after `self` in the same journal sequence.
    pub(crate) fn is_followed_by(&self, next: &Self) -> bool {
        self.sequence_id == next.sequence_id
            && self.sequence.get().checked_add(1) == Some(next.sequence.get())
    }
}
