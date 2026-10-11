//! The state a run hands the next run so that it need not fold the journal again.

use togi_defect::Entry;
use togi_journal::{Event, Journal, Seq, SessionId, StateError};

use crate::appender::Folds;
use crate::error::FoldError;
use crate::publication::{StateDocument, StateDocumentRef};

/// Every fold of a journal's events, owned, bound to the journal it was folded from: its session and its last event.
///
/// A run adopts it only when it covers exactly the journal it is given; any other journal is folded from its events.
#[derive(Clone, Debug, PartialEq)]
pub struct ResumeCapsule {
    session: Option<SessionId>,
    last: Option<Seq>,
    defects: &'static [Entry],
    folds: Folds,
}

/// The part of a capsule that is not what folding the journal's events gives.
#[derive(Debug, thiserror::Error)]
pub enum Divergence {
    #[error("the events do not fold: {0}")]
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
    #[error("the published state document {0}")]
    Document(#[from] DocumentDivergence),
}

/// How the state document published from a capsule's folds fails to be the one a cold fold publishes.
#[derive(Debug, thiserror::Error)]
pub enum DocumentDivergence {
    #[error("does not encode: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("does not decode: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("decodes to a different document")]
    RoundTrip,
    #[error("differs from the cold fold's")]
    Cold,
}

impl ResumeCapsule {
    pub(crate) fn new(journal: &Journal, defects: &'static [Entry], folds: Folds) -> Self {
        Self {
            session: journal.session(),
            last: last_seq(journal.events()),
            defects,
            folds,
        }
    }

    /// The folds, when this capsule was made from exactly `journal` and judged by the same `defects`.
    pub(crate) fn adopt(self, journal: &Journal, defects: &'static [Entry]) -> Option<Folds> {
        (self.session == journal.session()
            && self.last == last_seq(journal.events())
            && std::ptr::eq(self.defects, defects))
        .then_some(self.folds)
    }

    /// Writes the state document a live run publishes after its last append, for a run that ended without publishing.
    ///
    /// # Errors
    ///
    /// The journal's state file write error.
    pub fn publish(&self, journal: &mut Journal) -> Result<(), StateError> {
        journal.write_state(&StateDocumentRef::of(&self.folds))
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
        check_document(&StateDocument::of(kept), &StateDocument::of(&cold))?;
        Ok(())
    }
}

fn check_document(kept: &StateDocument, cold: &StateDocument) -> Result<(), DocumentDivergence> {
    let text = serde_json::to_string(kept).map_err(DocumentDivergence::Encode)?;
    let decoded: StateDocument = serde_json::from_str(&text).map_err(DocumentDivergence::Decode)?;
    if decoded != *kept {
        return Err(DocumentDivergence::RoundTrip);
    }
    if kept != cold {
        return Err(DocumentDivergence::Cold);
    }
    Ok(())
}

fn last_seq(events: &[Event]) -> Option<Seq> {
    events.last().map(|event| event.seq)
}

#[cfg(test)]
mod tests;
