//! The journal invariant audit: whether a recorded session kept the rules of `docs/spec/tuner.md` and
//! `docs/spec/journal.md`, judged from its events alone and independently of the tuner's own predicates.
//!
//! Offsets the tuner chooses are [`togi_machine::TargetOffset`]s, which cannot leave [-50, 0] and are refused by the
//! journal's decoder, so a journal holding one is unreadable ([`AuditError`]) rather than a violation. Readbacks and
//! baselines report the hardware, which may hold firmware offsets outside that range, and are not bounded.

mod auditor;
mod marks;
mod violation;
mod writes;

use togi_journal::{DirError, Event, LookupError, StateDir};

pub use violation::{Check, Completion, JournalAudit, Source, Violation};

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error(transparent)]
    Archives(#[from] DirError),
    #[error(transparent)]
    Journal(#[from] LookupError),
}

/// Every violation of one journal's events, in event order.
pub fn audit(events: &[Event], completion: Completion) -> Vec<Violation> {
    auditor::audit(events, completion)
}

/// Audits every journal of a state directory, the archived sessions oldest first and then the live one. Archives may
/// end anywhere; `live` says whether the live journal's run is finished.
pub fn audit_recordings(dir: &StateDir, live: Completion) -> Result<Vec<JournalAudit>, AuditError> {
    let archives = dir.archives()?;
    let mut recordings = dir.recordings()?.into_iter();
    let mut audits = Vec::new();
    for session in archives {
        if let Some(recording) = recordings.next() {
            audits.push(JournalAudit {
                source: Source::Archived(session),
                violations: audit(recording.events(), Completion::InProgress),
            });
        }
    }
    if let Some(recording) = recordings.next() {
        audits.push(JournalAudit {
            source: Source::Live,
            violations: audit(recording.events(), live),
        });
    }
    Ok(audits)
}
