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
/// before the check spends its time.
pub struct RecordsFile<'a> {
    path: &'a Path,
    out: BufWriter<File>,
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
            })
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
