//! The zero-only transaction shared by session recovery and abnormal-exit recovery.

use std::fmt;

use crate::{CoreId, CoreSet, Error, ObservedOffset, Smu, SmuValidated, Stopped, error_chain};

#[derive(Debug)]
pub struct Readback {
    pub core: CoreId,
    pub result: Result<ObservedOffset, Error>,
}

impl Readback {
    pub fn is_zero(&self) -> bool {
        matches!(self.result, Ok(ObservedOffset(0)))
    }

    fn problem(&self) -> Option<String> {
        match &self.result {
            Ok(ObservedOffset(0)) => None,
            Ok(offset) => Some(format!("core {:02} reads {}", self.core, offset.0)),
            Err(error) => Some(format!(
                "core {:02} unreadable: {}",
                self.core,
                error_chain(error)
            )),
        }
    }
}

#[derive(Debug)]
#[must_use = "the zeroing may have failed or read back a nonzero offset, which has to be reported"]
pub struct Zeroed {
    pub zero: Result<(), Error>,
    pub readbacks: Vec<Readback>,
}

impl Zeroed {
    pub fn is_clean(&self) -> bool {
        self.zero.is_ok() && self.readbacks.iter().all(Readback::is_zero)
    }

    /// The machine crashed during the emergency, and nothing touched the SMU after it.
    pub fn crashed(&self) -> bool {
        matches!(self.zero, Err(Error::Crashed))
            || self
                .readbacks
                .iter()
                .any(|readback| matches!(readback.result, Err(Error::Crashed)))
    }
}

/// What the zeroing did, as the journal-failure report gives it: the zero's own failure, if any, and the readback.
impl fmt::Display for Zeroed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let problems: Vec<String> = self
            .readbacks
            .iter()
            .filter_map(Readback::problem)
            .collect();
        let status = match (problems.is_empty(), self.crashed()) {
            (true, false) => "readback all 0".to_owned(),
            (true, true) => "no readback after the crash".to_owned(),
            (false, false) => format!("readback: {}", problems.join(", ")),
            (false, true) => format!(
                "readback: {}; no readback after the crash",
                problems.join(", ")
            ),
        };
        match &self.zero {
            Ok(()) => write!(f, "every core set to CO 0 without an intent ({status})"),
            Err(error) => write!(
                f,
                "setting every core to CO 0 without an intent failed: {} ({status})",
                error_chain(error)
            ),
        }
    }
}

/// Zeroes every core with no intent and reads each core of `cores` back. `stopped` proves no workload runs and
/// `validated` that the SMU is the one togi expects. Nothing is appended or reported here. A crash ends every SMU
/// access at once.
#[expect(
    clippy::disallowed_methods,
    reason = "the one caller of the zero-only SMU seam, shared by session recovery and emergency-stop"
)]
pub fn zero_and_read_back(
    smu: &mut dyn Smu,
    validated: SmuValidated,
    stopped: &Stopped,
    cores: CoreSet,
) -> Zeroed {
    let zero = smu.emergency_zero(validated, stopped);
    if matches!(zero, Err(Error::Crashed)) {
        return Zeroed {
            zero,
            readbacks: Vec::new(),
        };
    }
    let mut readbacks = Vec::with_capacity(cores.len());
    for core in cores {
        let result = smu.offset(core);
        let crashed = matches!(result, Err(Error::Crashed));
        readbacks.push(Readback { core, result });
        if crashed {
            break;
        }
    }
    Zeroed { zero, readbacks }
}
