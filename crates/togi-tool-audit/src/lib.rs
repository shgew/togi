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

use togi_journal::{
    DirError, Event, FileError, LookupError, Recording, SessionId, StateDir, StateError,
};

pub use interrupt::{Holding, INTERRUPTED, Interrupt};
pub use report::{
    Finding, Kind, Record, RecordError, Report, Split, Status, audit as audit_directories,
    audit_directory, audit_holding, check_records, read_records, run_directory, timing,
};
pub use sweep::{Evidence, RunError, Sweep, SweepError, sweep_suite};
pub use violation::{Check, Completion, JournalAudit, Source, Violation};

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error(transparent)]
    Archives(#[from] DirError),
    #[error(transparent)]
    Archive(#[from] FileError),
    #[error(transparent)]
    Journal(#[from] LookupError),
    #[error("archived session {0} vanished while the directory was locked")]
    MissingArchive(SessionId),
    #[error(transparent)]
    State(StateError),
}

/// Every violation of one journal's events, in event order.
pub fn audit(events: &[Event], completion: Completion) -> Vec<Violation> {
    auditor::audit(events, completion)
}

/// Audits every journal of a state directory, the archived sessions oldest first and then the live one, whose
/// published `state.json` must also equal a fresh fold of it, and for a finished run be published at its last event.
/// Archives may end anywhere; `live` says whether the live journal's run is finished.
pub fn audit_recordings(dir: &StateDir, live: Completion) -> Result<Vec<JournalAudit>, AuditError> {
    let archives = dir.archives()?;
    let mut audits = Vec::with_capacity(archives.len() + 1);
    for session in archives {
        let Some(recording) = dir.archived_recording(&session)? else {
            return Err(AuditError::MissingArchive(session));
        };
        audits.push(JournalAudit {
            source: Source::Archived(session),
            violations: audit(recording.events(), Completion::InProgress),
        });
    }
    if let Some(recording) = live_recording(dir, audits.len())? {
        let mut violations = audit(recording.events(), live);
        violations.extend(snapshot::audit(dir, recording.events(), live)?);
        audits.push(JournalAudit {
            source: Source::Live,
            violations,
        });
    }
    Ok(audits)
}

fn live_recording(dir: &StateDir, archived: usize) -> Result<Option<Recording>, LookupError> {
    Ok(dir.recordings()?.into_iter().nth(archived))
}
