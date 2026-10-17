//! What the audit reports: each state directory's journals, their violations and what a lenient read left out.

use std::path::{Path, PathBuf};

use serde::Serialize;
use togi_journal::{LockError, Seq, Skipped, StateDir};

use crate::{Check, Completion, Source, audit_recordings};

/// What a finding is about: one of the journal [`Check`]s, or a directory the audit could not read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Audit(Check),
    Unreadable,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Audit(check) => check.as_str(),
            Self::Unreadable => "unreadable",
        }
    }
}

impl Serialize for Kind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// One violation, with where it was found. Serialized as one JSON object per line.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Finding {
    pub directory: PathBuf,
    /// `live` or the session id of an archive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<Seq>,
    pub check: Kind,
    pub reason: String,
}

/// Why a state directory could not be opened for an audit.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("not a directory")]
    NotADirectory,
    #[error(transparent)]
    Lock(#[from] LockError),
}

/// What a lenient read left out of one journal of a state directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalSkip {
    pub directory: PathBuf,
    /// `live`, or the archived session's id.
    pub journal: String,
    pub skipped: Skipped,
}

/// The journals of one state directory: how many were read, what they violate and what was left out of them.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct DirectoryAudit {
    pub journals: usize,
    pub findings: Vec<Finding>,
    pub skipped: Vec<JournalSkip>,
}

/// Audits the journals of one state directory; `completion` says whether its live journal's run is finished, which
/// must then have concluded or reached a dead end. The audit holds the directory's writer lock, so a directory a
/// live process holds is refused. Taking the lock creates the directory's `lock` file and sets the directory's mode
/// to 0711; the audit writes nothing else.
pub fn audit_directory(dir: &Path, completion: Completion) -> DirectoryAudit {
    audit_directory_with(dir, completion, |dir| {
        if !dir.is_dir() {
            return Err(OpenError::NotADirectory);
        }
        Ok(StateDir::lock(dir)?)
    })
}

/// Audits a directory through an injected state-directory opener, retaining the same finding attribution.
pub fn audit_directory_with(
    dir: &Path,
    completion: Completion,
    open: impl FnOnce(&Path) -> Result<StateDir, OpenError>,
) -> DirectoryAudit {
    let unreadable = |reason: String| Finding {
        directory: dir.to_path_buf(),
        journal: None,
        seq: None,
        check: Kind::Unreadable,
        reason,
    };
    let audits = match open(dir) {
        Ok(state) => {
            audit_recordings(&state, completion).map_err(|error| togi_machine::error_chain(&error))
        }
        Err(error) => Err(togi_machine::error_chain(&error)),
    };
    let audits = match audits {
        Ok(audits) => audits,
        Err(reason) => {
            return DirectoryAudit {
                findings: vec![unreadable(reason)],
                ..DirectoryAudit::default()
            };
        }
    };
    let journals = audits.len();
    let mut found = Vec::new();
    let mut skips = Vec::new();
    for audit in audits {
        let journal = match audit.source {
            Source::Archived(session) => session.to_string(),
            Source::Live => "live".into(),
        };
        if !audit.skipped.is_empty() {
            skips.push(JournalSkip {
                directory: dir.to_path_buf(),
                journal: journal.clone(),
                skipped: audit.skipped,
            });
        }
        for violation in audit.violations {
            found.push(Finding {
                directory: dir.to_path_buf(),
                journal: Some(journal.clone()),
                seq: Some(violation.seq),
                check: Kind::Audit(violation.check),
                reason: violation.reason,
            });
        }
    }
    DirectoryAudit {
        journals,
        findings: found,
        skipped: skips,
    }
}

/// The outcome of auditing a set of directories.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub directories: usize,
    pub journals: usize,
    pub findings: Vec<Finding>,
    pub skipped: Vec<JournalSkip>,
}

/// Audits `dirs` in order, each as a run that may still be in progress.
pub fn audit(dirs: &[PathBuf]) -> Report {
    audit_with(dirs, |dir| audit_directory(dir, Completion::InProgress))
}

/// Audits `dirs` in order through an injected directory audit.
pub fn audit_with(dirs: &[PathBuf], inspect: impl Fn(&Path) -> DirectoryAudit) -> Report {
    let mut report = Report {
        directories: dirs.len(),
        ..Report::default()
    };
    for dir in dirs {
        let mut audited = inspect(dir);
        report.journals += audited.journals;
        report.findings.append(&mut audited.findings);
        report.skipped.append(&mut audited.skipped);
    }
    report
}
