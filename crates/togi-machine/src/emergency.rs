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
enum Readbacks {
    Complete(Vec<Readback>),
    Incomplete(Vec<Readback>),
}

#[derive(Debug)]
#[must_use = "the zeroing may have failed or read back a nonzero offset, which has to be reported"]
pub struct Zeroed {
    pub zero: Result<(), Error>,
    readbacks: Readbacks,
}

impl Zeroed {
    fn new(zero: Result<(), Error>, cores: CoreSet, readbacks: Vec<Readback>) -> Self {
        let complete = !cores.is_empty() && cores.into_iter().eq(readbacks.iter().map(|r| r.core));
        let readbacks = if complete {
            Readbacks::Complete(readbacks)
        } else {
            Readbacks::Incomplete(readbacks)
        };
        Self { zero, readbacks }
    }

    pub fn readbacks(&self) -> &[Readback] {
        match &self.readbacks {
            Readbacks::Complete(readbacks) | Readbacks::Incomplete(readbacks) => readbacks,
        }
    }

    pub fn is_clean(&self) -> bool {
        let Readbacks::Complete(readbacks) = &self.readbacks else {
            return false;
        };
        self.zero.is_ok() && readbacks.iter().all(Readback::is_zero)
    }

    /// The machine crashed during the emergency, and nothing touched the SMU after it.
    pub fn crashed(&self) -> bool {
        matches!(self.zero, Err(Error::Crashed))
            || self
                .readbacks()
                .iter()
                .any(|readback| matches!(readback.result, Err(Error::Crashed)))
    }
}

/// What the zeroing did, as the journal-failure report gives it: the zero's own failure, if any, and the readback.
impl fmt::Display for Zeroed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let problems: Vec<String> = self
            .readbacks()
            .iter()
            .filter_map(Readback::problem)
            .collect();
        let status = match (problems.is_empty(), self.crashed()) {
            (true, false) if matches!(self.readbacks, Readbacks::Incomplete(_)) => {
                "incomplete readback".to_owned()
            }
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
        return Zeroed::new(zero, cores, Vec::new());
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
    Zeroed::new(zero, cores, readbacks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(0 => false; "empty")]
    #[test_case(1 => false; "short")]
    #[test_case(2 => true; "complete")]
    fn clean_zero_requires_every_readback(count: usize) -> bool {
        let cores: CoreSet = [CoreId::from_index(0), CoreId::from_index(2)]
            .into_iter()
            .collect();
        let readbacks = cores
            .iter()
            .take(count)
            .map(|core| Readback {
                core,
                result: Ok(ObservedOffset(0)),
            })
            .collect();
        Zeroed::new(Ok(()), cores, readbacks).is_clean()
    }
}
