//! The state a run hands the next run so that it need not fold the journal again.

use togi_defect::Entry;
use togi_journal::{Event, Journal, OpenPosition, Seq};

use crate::appender::Folds;
use crate::error::FoldError;

/// The live folds of one open journal at its last event.
///
/// A run adopts them only when it is given the same journal at that position, or replays its events.
#[derive(Clone, Debug, PartialEq)]
pub struct ResumeCapsule {
    last: Option<Seq>,
    defects: &'static [Entry],
    folds: Folds,
    position: OpenPosition,
}

/// The part of a capsule that is not what folding the journal's events gives.
#[derive(Debug, thiserror::Error)]
pub enum Divergence {
    #[error("the events do not fold")]
    Fold(#[from] FoldError),
    #[error("the capsule's last event is {capsule:?}, the journal's is {journal:?}")]
    Last {
        capsule: Option<Seq>,
        journal: Option<Seq>,
    },
    #[error("the journal facts differ")]
    Facts,
    #[error("the tuner state differs")]
    Tuner,
    #[error("the recovery state differs")]
    Recovery,
    #[error("the defect state differs")]
    Defects,
    #[error("the presentation state differs")]
    Presentation,
}

impl ResumeCapsule {
    pub(crate) fn new(journal: &Journal, defects: &'static [Entry], folds: Folds) -> Self {
        Self {
            last: last_seq(journal.events()),
            defects,
            folds,
            position: journal.position(),
        }
    }

    /// Whether this capsule was made from exactly `journal` and judged by the same `defects`.
    #[must_use]
    pub fn covers(&self, journal: &Journal, defects: &'static [Entry]) -> bool {
        self.position == journal.position() && std::ptr::eq(self.defects, defects)
    }

    /// The folds, when [`ResumeCapsule::covers`] the journal.
    pub(crate) fn adopt(self, journal: &Journal, defects: &'static [Entry]) -> Option<Folds> {
        self.covers(journal, defects).then_some(self.folds)
    }

    /// Checks every part against the fold of `events` from nothing, naming the first part that differs.
    ///
    /// # Errors
    ///
    /// The part that differs, or the fold error of events that cannot be folded.
    pub fn verify(&self, events: &[Event]) -> Result<(), Divergence> {
        let cold = Folds::replay_with(self.defects, events)?;
        let last = last_seq(events);
        if self.last != last {
            return Err(Divergence::Last {
                capsule: self.last,
                journal: last,
            });
        }
        let kept = &self.folds;
        if kept.facts != cold.facts {
            return Err(Divergence::Facts);
        }
        if kept.tuner != cold.tuner {
            return Err(Divergence::Tuner);
        }
        if kept.recovery != cold.recovery {
            return Err(Divergence::Recovery);
        }
        if kept.defects != cold.defects {
            return Err(Divergence::Defects);
        }
        if kept.presentation != cold.presentation {
            return Err(Divergence::Presentation);
        }
        Ok(())
    }
}

fn last_seq(events: &[Event]) -> Option<Seq> {
    events.last().map(|event| event.seq)
}

#[cfg(test)]
mod tests;
