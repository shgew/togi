//! The forward-chained check's full-precision records: one JSON line for the input, one per scored held-out
//! trial, one per held-out session and one for the pool. The text report rounds; these keep every value
//! exactly as computed, so two runs on the same input can be paired trial by trial ([`crate::compare`]).

use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use serde::Serialize;
use togi_journal::{Seq, SessionId};
use togi_machine::{WorkloadId, object_only};

use crate::artifact::Extract;
use crate::error::Error;
use crate::forward::{ForwardCheck, ForwardScore};

object_only! {
    /// One line of a records file.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Record {
        // The rows the check read and the sessions it sealed.
        Input {
            sha256: String,
            rows: usize,
            seal: usize,
        },
        // One scored held-out trial, identified by `session` and `seq`; `probability` is the predicted
        // failure probability before the log-loss clamp.
        Trial {
            session: SessionId,
            seq: Seq,
            training_sessions: usize,
            workload: WorkloadId,
            loaded: usize,
            failure: bool,
            probability: f64,
            constant: f64,
            fit_log_loss: f64,
            constant_log_loss: f64,
            surprise: bool,
            matched: bool,
        },
        // A held-out session's sums, which its trial lines add up to in file order.
        Session {
            session: SessionId,
            ruleset: u32,
            training_sessions: usize,
            constant: f64,
            sums: Sums,
        },
        // The sums over every held-out session, in session order.
        Pooled {
            sums: Sums,
        },
    }
}

object_only! {
    /// A [`ForwardScore`] at full precision; the log losses are sums, not per-trial means.
    #[derive(Clone, Copy, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Sums {
        pub trials: usize,
        pub failures: usize,
        pub predicted: f64,
        pub fit_log_loss: f64,
        pub constant_log_loss: f64,
        pub surprises: usize,
        pub matches: usize,
        pub flagged: usize,
        pub eligible: usize,
    }
}

impl From<&ForwardScore> for Sums {
    fn from(score: &ForwardScore) -> Self {
        Self {
            trials: score.counts.trials,
            failures: score.counts.failures,
            predicted: score.counts.predicted,
            fit_log_loss: score.counts.fit_log_loss,
            constant_log_loss: score.counts.constant_log_loss,
            surprises: score.surprises,
            matches: score.matches,
            flagged: score.flagged,
            eligible: score.eligible,
        }
    }
}

/// Writes the records of `check`, run on `extract` with the newest `seal` sessions sealed, as JSON Lines.
pub fn encode(
    out: &mut impl Write,
    extract: &Extract,
    seal: usize,
    check: &ForwardCheck,
) -> io::Result<()> {
    let mut line = |record: &Record| -> io::Result<()> {
        serde_json::to_writer(&mut *out, record)?;
        out.write_all(b"\n")
    };
    line(&Record::Input {
        sha256: extract.sha256.clone(),
        rows: extract.rows,
        seal,
    })?;
    for row in &check.rows {
        for trial in &row.trials {
            line(&Record::Trial {
                session: trial.session,
                seq: trial.seq,
                training_sessions: row.training_sessions,
                workload: trial.workload,
                loaded: trial.loaded,
                failure: trial.failure,
                probability: trial.probability,
                constant: row.constant,
                fit_log_loss: trial.fit_log_loss,
                constant_log_loss: trial.constant_log_loss,
                surprise: trial.surprise,
                matched: trial.matched,
            })?;
        }
        line(&Record::Session {
            session: row.session,
            ruleset: row.ruleset,
            training_sessions: row.training_sessions,
            constant: row.constant,
            sums: Sums::from(&row.score),
        })?;
    }
    line(&Record::Pooled {
        sums: Sums::from(&check.pooled),
    })
}

/// A new records file, created before the forward-chained check runs so a path that cannot take it fails
/// before the check spends its time. Dropped unwritten, because the check, its report or the write failed,
/// it removes the file again, so a failed run leaves no records behind.
pub struct RecordsFile<'a> {
    path: &'a Path,
    out: BufWriter<File>,
    written: bool,
}

impl<'a> RecordsFile<'a> {
    /// Creates the new file `path`; an existing file is never replaced.
    pub fn create(path: &'a Path) -> Result<Self, Error> {
        let file = File::create_new(path).map_err(|source| Error::Io {
            action: "create the forward records",
            path: path.to_owned(),
            source,
        })?;
        Ok(Self {
            path,
            out: BufWriter::new(file),
            written: false,
        })
    }

    /// Writes the records of `check`, run on `extract` with the newest `seal` sessions sealed.
    pub fn write(
        mut self,
        extract: &Extract,
        seal: usize,
        check: &ForwardCheck,
    ) -> Result<(), Error> {
        encode(&mut self.out, extract, seal, check)
            .and_then(|()| self.out.flush())
            .map_err(|source| Error::Io {
                action: "write the forward records",
                path: self.path.to_owned(),
                source,
            })?;
        self.written = true;
        Ok(())
    }
}

impl Drop for RecordsFile<'_> {
    fn drop(&mut self) {
        if !self.written {
            // The run is already failing with its own error, which this one must not replace; a file left
            // behind is empty or cut short, and `compare` refuses it.
            let _ = std::fs::remove_file(self.path);
        }
    }
}

/// Parses records written by [`encode`]; `path` names them in errors.
pub fn decode(path: &Path, reader: impl BufRead) -> Result<Vec<Record>, Error> {
    let mut records = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|source| Error::Io {
            action: "read the forward records",
            path: path.to_owned(),
            source,
        })?;
        records.push(
            serde_json::from_str(&line).map_err(|source| Error::Records {
                path: path.to_owned(),
                line: index + 1,
                source,
            })?,
        );
    }
    Ok(records)
}

/// Reads a records file written by [`write`].
pub fn read(path: &Path) -> Result<Vec<Record>, Error> {
    let file = File::open(path).map_err(|source| Error::Io {
        action: "open the forward records",
        path: path.to_owned(),
        source,
    })?;
    decode(path, BufReader::new(file))
}

/// Trial and failure counts, as a summary records them and as its trial lines add up.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Totals {
    pub trials: usize,
    pub failures: usize,
}

impl Totals {
    fn of(sums: &Sums) -> Self {
        Self {
            trials: sums.trials,
            failures: sums.failures,
        }
    }

    fn add(&mut self, other: Self) {
        self.trials = self.trials.saturating_add(other.trials);
        self.failures = self.failures.saturating_add(other.failures);
    }
}

impl std::fmt::Display for Totals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} trials and {} failures", self.trials, self.failures)
    }
}

/// Why records are not the whole output of [`encode`]; `line` counts from 1, as [`decode`] does.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum Defect {
    #[error("it does not begin with the input line")]
    NoInput,
    #[error("line {line} is a second input")]
    SecondInput { line: usize },
    #[error("line {line} names another session than the trial lines of its section")]
    WrongSession { line: usize },
    #[error("line {line} names a session whose summary already closed it")]
    RepeatedSession { line: usize },
    #[error("the session at line {line} records {recorded} where its trial lines hold {counted}")]
    SessionTotals {
        line: usize,
        recorded: Totals,
        counted: Totals,
    },
    #[error("{trials} trial lines follow the last session summary")]
    UnsummarizedTrials { trials: usize },
    #[error(
        "the pooled summary at line {line} records {recorded} where the sessions hold {counted}"
    )]
    PooledTotals {
        line: usize,
        recorded: Totals,
        counted: Totals,
    },
    #[error("it ends without the pooled summary")]
    NoPooled,
    #[error("line {line} is a second pooled summary")]
    SecondPooled { line: usize },
    #[error("line {line} follows the pooled summary")]
    AfterPooled { line: usize },
}

/// Requires `records` to be what [`encode`] writes: the input, each held-out session's trial lines
/// followed by its summary, then exactly one pooled summary and nothing after it, with every summary's
/// trial and failure counts equal to those of the trial lines it closes. A file cut short, or a summary
/// that disagrees with its trials, is refused.
pub fn require_complete(records: &[Record]) -> Result<(), Defect> {
    if !matches!(records.first(), Some(Record::Input { .. })) {
        return Err(Defect::NoInput);
    }
    let mut section = Totals::default();
    let mut section_session = None;
    let mut pool = Totals::default();
    let mut pooled = false;
    let mut closed = std::collections::BTreeSet::new();
    for (index, record) in records.iter().enumerate().skip(1) {
        let line = index.saturating_add(1);
        if pooled {
            return Err(match record {
                Record::Pooled { .. } => Defect::SecondPooled { line },
                Record::Input { .. } | Record::Trial { .. } | Record::Session { .. } => {
                    Defect::AfterPooled { line }
                }
            });
        }
        match record {
            Record::Input { .. } => return Err(Defect::SecondInput { line }),
            Record::Trial {
                session, failure, ..
            } => {
                if closed.contains(session) {
                    return Err(Defect::RepeatedSession { line });
                }
                if section_session.is_some_and(|open| open != *session) {
                    return Err(Defect::WrongSession { line });
                }
                section_session = Some(*session);
                section.add(Totals {
                    trials: 1,
                    failures: usize::from(*failure),
                });
            }
            Record::Session { session, sums, .. } => {
                if closed.contains(session) {
                    return Err(Defect::RepeatedSession { line });
                }
                if section_session.is_some_and(|open| open != *session) {
                    return Err(Defect::WrongSession { line });
                }
                let recorded = Totals::of(sums);
                if recorded != section {
                    return Err(Defect::SessionTotals {
                        line,
                        recorded,
                        counted: section,
                    });
                }
                pool.add(section);
                section = Totals::default();
                section_session = None;
                closed.insert(*session);
            }
            Record::Pooled { sums } => {
                if section.trials > 0 {
                    return Err(Defect::UnsummarizedTrials {
                        trials: section.trials,
                    });
                }
                let recorded = Totals::of(sums);
                if recorded != pool {
                    return Err(Defect::PooledTotals {
                        line,
                        recorded,
                        counted: pool,
                    });
                }
                pooled = true;
            }
        }
    }
    if pooled {
        Ok(())
    } else {
        Err(Defect::NoPooled)
    }
}
