//! What a sweep audits and reports: the bench's run records for bench-status and wall-time checks, and every state
//! directory's journals.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use togi_journal::{Seq, Skipped, StateDir};
use togi_machine::object_only;

use crate::{Check, Completion, Source, audit_recordings};

/// A wall time more than this many times its scenario's median is a hang that finished.
const WALL_TIME_RATIO: f64 = 10.0;

/// What a finding is about: one of the journal [`Check`]s, or a property of the run the bench measured.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Audit(Check),
    WallTime,
    BenchStatus,
    Unreadable,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Audit(check) => check.as_str(),
            Self::WallTime => "wall_time",
            Self::BenchStatus => "bench_status",
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// `live` or the session id of an archive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<Seq>,
    pub check: Kind,
    pub reason: String,
}

object_only! {
    /// The members of a bench run record the audit reads; the bench's other members are ignored, so the audit
    /// follows the bench's file by its subprocess interface alone.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Record {
        pub scenario: String,
        pub seed: u64,
        pub split: Split,
        pub status: Status,
        pub wall_s: f64,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Dev,
    Holdout,
}

impl Split {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Holdout => "holdout",
        }
    }
}

/// How a bench run ended, as the bench's record spells it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Concluded,
    DeadEnd,
    Censored,
    Error,
}

/// Reads one record per line; blank lines are skipped.
pub fn read_records(path: &Path) -> Result<Vec<Record>, RecordError> {
    let file = std::fs::File::open(path).map_err(|source| RecordError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    read_records_from(path, file)
}

/// Reads and validates run records from a reader; `path` labels read and decode errors.
pub fn read_records_from(path: &Path, mut reader: impl Read) -> Result<Vec<Record>, RecordError> {
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .map_err(|source| RecordError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if !line.trim().is_empty() {
            records.push(
                serde_json::from_str(line).map_err(|source| RecordError::Decode {
                    path: path.to_path_buf(),
                    line: index + 1,
                    source,
                })?,
            );
        }
    }
    check_records(&records)?;
    Ok(records)
}

/// The records of a sweep or bench run do not describe runs this tool can find.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("open run records {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decode run records {} line {line}: {source}", path.display())]
    Decode {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error("run records are empty")]
    Empty,
    #[error("invalid run record for scenario {scenario:?} seed {seed}")]
    Invalid { scenario: String, seed: u64 },
}

/// The directory the bench kept a run's state in, below its run root.
pub fn run_directory(root: &Path, record: &Record) -> PathBuf {
    root.join(&record.scenario)
        .join(format!("{}-{}", record.split.as_str(), record.seed))
}

/// Refuses records that are empty, name a scenario that is not one path component, or took negative time.
pub fn check_records(records: &[Record]) -> Result<(), RecordError> {
    if records.is_empty() {
        return Err(RecordError::Empty);
    }
    for record in records {
        let name = Path::new(&record.scenario);
        let one_component = name.components().count() == 1
            && name
                .file_name()
                .is_some_and(|file| file == name.as_os_str());
        if !one_component || !record.wall_s.is_finite() || record.wall_s < 0.0 {
            return Err(RecordError::Invalid {
                scenario: record.scenario.clone(),
                seed: record.seed,
            });
        }
    }
    Ok(())
}

/// Wall-time and bench-status violations of `records`, in record order: a run over ten times its scenario's median
/// wall time, and a run that neither concluded nor reached a dead end.
pub fn timing(records: &[Record], root: &Path) -> Vec<Finding> {
    let mut times: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for record in records {
        times
            .entry(&record.scenario)
            .or_default()
            .push(record.wall_s);
    }
    let medians: BTreeMap<&str, f64> = times
        .into_iter()
        .map(|(name, mut values)| {
            values.sort_by(f64::total_cmp);
            let middle = values.len() / 2;
            let median = if values.len() % 2 == 0 {
                f64::midpoint(values[middle - 1], values[middle])
            } else {
                values[middle]
            };
            (name, median)
        })
        .collect();
    let mut found = Vec::new();
    for record in records {
        let finding = |check, reason| Finding {
            directory: run_directory(root, record),
            scenario: Some(record.scenario.clone()),
            seed: Some(record.seed),
            journal: None,
            seq: None,
            check,
            reason,
        };
        let median = medians[record.scenario.as_str()];
        if record.wall_s > WALL_TIME_RATIO * median {
            found.push(finding(
                Kind::WallTime,
                format!(
                    "wall time {:.3}s exceeds {WALL_TIME_RATIO}x scenario median {median:.3}s",
                    record.wall_s
                ),
            ));
        }
        if !matches!(record.status, Status::Concluded | Status::DeadEnd) {
            found.push(finding(
                Kind::BenchStatus,
                format!("bench run ended with status {}", status(record.status)),
            ));
        }
    }
    found
}

const fn status(status: Status) -> &'static str {
    match status {
        Status::Concluded => "concluded",
        Status::DeadEnd => "deadend",
        Status::Censored => "censored",
        Status::Error => "error",
    }
}

/// What a lenient read left out of one journal of a state directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalSkip {
    pub directory: PathBuf,
    /// `live`, or the archived session's id.
    pub journal: String,
    pub skipped: Skipped,
}

/// The journals of one state directory: how many were read, what they violate and what was left out of them. The
/// live journal of a simulated run must also have concluded. The audit holds the directory's writer lock, so a
/// directory a live process holds is refused. Taking the lock creates the directory's `lock` file and sets the
/// directory's mode to 0711; the audit writes nothing else.
pub fn audit_directory(dir: &Path, simulated: bool) -> (Vec<Finding>, usize, Vec<JournalSkip>) {
    audit_directory_with(dir, simulated, |dir| {
        if !dir.is_dir() {
            return Err("not a directory".into());
        }
        StateDir::lock(dir).map_err(|error| togi_machine::error_chain(&error))
    })
}

/// Audits a directory through an injected state-directory opener, retaining the same finding attribution.
pub fn audit_directory_with(
    dir: &Path,
    simulated: bool,
    open: impl FnOnce(&Path) -> Result<StateDir, String>,
) -> (Vec<Finding>, usize, Vec<JournalSkip>) {
    let unreadable = |reason: String| Finding {
        directory: dir.to_path_buf(),
        scenario: None,
        seed: None,
        journal: None,
        seq: None,
        check: Kind::Unreadable,
        reason,
    };
    let completion = if simulated {
        Completion::Finished
    } else {
        Completion::InProgress
    };
    let audits = open(dir).and_then(|state| {
        audit_recordings(&state, completion).map_err(|error| togi_machine::error_chain(&error))
    });
    let audits = match audits {
        Ok(audits) => audits,
        Err(reason) => return (vec![unreadable(reason)], 0, Vec::new()),
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
                scenario: None,
                seed: None,
                journal: Some(journal.clone()),
                seq: Some(violation.seq),
                check: Kind::Audit(violation.check),
                reason: violation.reason,
            });
        }
    }
    (found, journals, skips)
}

/// The outcome of auditing a set of directories.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub directories: usize,
    pub journals: usize,
    pub findings: Vec<Finding>,
    pub skipped: Vec<JournalSkip>,
}

/// Audits `dirs`, attributing each directory a record keeps to that run, after the records' own timing checks.
/// Every spelling of one directory, relative or absolute, with `.` or `..` components, shares a record.
pub fn audit(dirs: &[PathBuf], records: &[Record], root: &Path) -> std::io::Result<Report> {
    audit_holding(dirs, records, root, || ())
}

/// [`audit`], with `hold()` called before each directory is audited and its result kept until that directory is done.
pub fn audit_holding<G>(
    dirs: &[PathBuf],
    records: &[Record],
    root: &Path,
    hold: impl Fn() -> G,
) -> std::io::Result<Report> {
    audit_with(dirs, records, root, hold, audit_directory)
}

/// Audits directories using an injected journal inspection and per-directory holding hook.
/// Absolute fixture paths avoid consulting the process's current directory during lexical attribution.
pub fn audit_with<G>(
    dirs: &[PathBuf],
    records: &[Record],
    root: &Path,
    hold: impl Fn() -> G,
    inspect: impl Fn(&Path, bool) -> (Vec<Finding>, usize, Vec<JournalSkip>),
) -> std::io::Result<Report> {
    let mut by_dir = BTreeMap::new();
    for record in records {
        by_dir.insert(lexical(&run_directory(root, record))?, record);
    }
    let mut report = Report {
        directories: dirs.len(),
        findings: timing(records, root),
        ..Report::default()
    };
    for dir in dirs {
        let record = by_dir.get(&lexical(dir)?);
        let guard = hold();
        let (mut found, journals, mut skips) = inspect(dir, record.is_some());
        drop(guard);
        report.journals += journals;
        for finding in &mut found {
            if let Some(record) = record {
                finding.scenario = Some(record.scenario.clone());
                finding.seed = Some(record.seed);
            }
        }
        report.findings.append(&mut found);
        report.skipped.append(&mut skips);
    }
    Ok(report)
}

/// `path` made absolute with its `.` and `..` components resolved by name alone, as the bench names its run
/// directories; a symbolic link followed by `..` is not resolved.
fn lexical(path: &Path) -> std::io::Result<PathBuf> {
    let mut resolved = PathBuf::new();
    for component in std::path::absolute(path)?.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                resolved.push(other);
            }
        }
    }
    Ok(resolved)
}
