//! The journal invariant audit: whether a recorded session kept the rules of `docs/spec/tuner.md` and
//! `docs/spec/journal.md`, judged from its events alone and independently of the tuner's own predicates. Only the
//! check of a published `state.json` replays the journal through the tuner, since that document is the tuner's
//! projection.
//!
//! Offsets the tuner chooses are [`togi_machine::TargetOffset`]s, which cannot leave [-50, 0] and are refused by the
//! journal's decoder, so a journal holding one is unreadable ([`AuditError`]) rather than a violation. Readbacks and
//! baselines report the hardware, which may hold firmware offsets outside that range, and are not bounded.
//!
//! The `togi-tool-audit` command sweeps: it runs `togi-tool-bench` over a generated suite of every scenario with
//! seeds `1..=N`, keeps each run's state directory, and audits them with the runs' bench statuses and wall times. Given
//! state directories instead, it audits those.

mod auditor;
mod interrupt;
mod marks;
mod report;
mod snapshot;
mod sweep;
mod violation;
mod writes;

use togi_journal::{DirError, Event, LookupError, Skipped, StateDir, StateError};

pub use interrupt::{Holding, INTERRUPTED, Interrupt};
pub use report::{
    Finding, JournalSkip, Kind, OpenError, Record, RecordError, Report, Split, Status,
    audit as audit_directories, audit_directory, audit_directory_with, audit_holding, audit_with,
    check_records, read_records, read_records_from, run_directory, timing,
};
pub use sweep::{Evidence, RunError, Sweep, SweepError, sweep_suite};
pub use violation::{Check, Completion, JournalAudit, Source, Violation};

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error(transparent)]
    Archives(#[from] DirError),
    #[error(transparent)]
    Journal(#[from] LookupError),
    #[error(transparent)]
    State(StateError),
}

/// Every violation of one journal's events, in event order.
pub fn audit(events: &[Event], completion: Completion) -> Vec<Violation> {
    audit_skipping(events, &Skipped::default(), completion)
}

/// [`audit`] of events a lenient read left holes in: the sequences `skipped` names are not violations, and a cause
/// naming one is not a bad cause.
pub fn audit_skipping(
    events: &[Event],
    skipped: &Skipped,
    completion: Completion,
) -> Vec<Violation> {
    auditor::audit(events, skipped, completion)
}

/// Audits every journal of a state directory, the archived sessions oldest first and then the live one, whose
/// published `state.json` must also equal a fresh fold of it, and for a finished run be published at its last event.
/// Archives may end anywhere; `live` says whether the live journal's run is finished. Journals are read leniently:
/// events of kinds this build does not know are left out and reported in each [`JournalAudit::skipped`].
pub fn audit_recordings(dir: &StateDir, live: Completion) -> Result<Vec<JournalAudit>, AuditError> {
    let archives = dir.archives()?;
    let mut recordings = dir.recordings_lenient()?.into_iter();
    let mut audits = Vec::with_capacity(archives.len() + 1);
    for (session, recording) in archives.into_iter().zip(recordings.by_ref()) {
        audits.push(JournalAudit {
            source: Source::Archived(session),
            violations: audit_skipping(
                recording.events(),
                recording.skipped(),
                Completion::InProgress,
            ),
            skipped: recording.skipped().clone(),
        });
    }
    if let Some(recording) = recordings.next() {
        let mut violations = audit_skipping(recording.events(), recording.skipped(), live);
        violations.extend(snapshot::audit(dir, recording.events(), live)?);
        audits.push(JournalAudit {
            source: Source::Live,
            violations,
            skipped: recording.skipped().clone(),
        });
    }
    Ok(audits)
}
