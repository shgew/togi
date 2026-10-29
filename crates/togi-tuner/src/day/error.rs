use crate::UnitId;
use togi_machine::InvalidTarget;

use super::profile::CoreProfileError;

/// Why the day policy could not decide: its inputs do not describe one machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DayStepError {
    #[error("comparing a profile on the topology's cores")]
    Profile(#[from] CoreProfileError),
    #[error("moving an offset left the valid range")]
    Offset(#[from] InvalidTarget),
    #[error("unit {0:?} is missing from a profile the decision compares")]
    Unit(UnitId),
}
