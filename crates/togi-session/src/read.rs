//! The session's reducers replayed from recorded events.

use togi_journal::Event;

use crate::FoldError;
use crate::appender::Folds;

/// Complete folded state for readers.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadState {
    folds: Folds,
}

impl ReadState {
    /// Folds the journal from the beginning.
    ///
    /// # Errors
    ///
    /// Returns the journal or tuner fold failure.
    pub fn replay(events: &[Event]) -> Result<Self, FoldError> {
        Folds::replay(events).map(|folds| Self { folds })
    }

    /// Shared journal facts at the end of the journal.
    pub const fn facts(&self) -> &togi_journal::State {
        &self.folds.facts
    }

    /// The complete incremental tuner state.
    pub const fn tuner(&self) -> &togi_tuner::policy::State {
        &self.folds.tuner
    }

    /// The current defect index.
    pub const fn defects(&self) -> &togi_defect::DefectState {
        &self.folds.defects
    }
}
