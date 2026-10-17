use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufWriter, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use togi_machine::{ObservedOffset, PerCore, Regime, object_only};

use crate::error::Error;
use togi_tool_forecast::Split;

/// How a run ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// The simulator stopped for its clean cycle.
    Concluded,
    /// The session stopped at a dead end.
    DeadEnd,
    /// The simulator reached its boot cap first; the run's time and crashes are lower bounds.
    Censored,
    /// The simulator failed, or left no journal.
    Error,
}

impl Status {
    /// The status of a simulator that exited with `exit`, given what its session left behind: whether its journal
    /// holds events, whether one is a dead end, and whether its log reports one.
    pub const fn of(
        exit: i32,
        has_events: bool,
        journal_dead_end: bool,
        log_dead_end: bool,
    ) -> Self {
        match exit {
            0 if has_events => Self::Concluded,
            1 if journal_dead_end || log_dead_end => Self::DeadEnd,
            3 if has_events => Self::Censored,
            _ => Self::Error,
        }
    }
}

object_only! {
    /// The model check of the run's machine against its facts extract, reduced to its verdict; the flagged groups
    /// are in the report.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ModelCheck {
        pub eligible_groups: usize,
        pub flagged_groups: usize,
        pub idle_failures: usize,
    }

    /// The measurements of one simulated session, with the identity of the run and the tree that made it.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Record {
        pub scenario: String,
        /// The machine file as the suite names it; none for the seeded default machine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub machine: Option<String>,
        pub seed: u64,
        pub split: Split,
        /// The short revision of the tree the bench ran from.
        pub commit: String,
        /// Whether that tree had uncommitted changes.
        pub dirty: bool,
        pub ruleset: u32,
        pub evidence: u32,
        pub status: Status,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub model_check: Option<ModelCheck>,
        pub exit_code: i32,
        /// Real seconds the simulator took; a cached record keeps the wall time of the run that made it. A baseline
        /// file leaves it out, and no comparison reads it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub wall_s: Option<f64>,
        pub sim_hours: f64,
        #[serde(deserialize_with = "Option::deserialize")]
        pub first_passed_cycle_h: Option<f64>,
        pub passed_cycles: u32,
        pub crashes: u32,
        /// Trials that ended, whatever the outcome.
        pub trials: u32,
        /// Decisive trials a real fact answered.
        pub real_answers: u32,
        pub real_answer_share: f64,
        pub scenario_real_answer_share: f64,
        pub trial_hours: f64,
        pub hunts: u32,
        pub combinations: u32,
        pub final_profile: PerCore<ObservedOffset>,
        /// The sum of the final profile's offsets: negative, deeper is more negative.
        pub depth: i64,
        /// Steady-state failures per hour with every core loaded, by regime.
        pub hazard_per_h: BTreeMap<Regime, f64>,
        pub hazard_max_per_h: f64,
        /// The largest shared-voltage hazard over the R7 loads; absent on a machine without a shared rail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub worst_r7_hazard_per_h: Option<f64>,
    }
}

impl Record {
    pub(crate) fn key(&self) -> (&str, u64) {
        (&self.scenario, self.seed)
    }

    /// The record as a baseline file keeps it: without the model check, which every run of its machine repeats,
    /// and without the wall time. Comparison and gates read neither.
    #[must_use]
    pub fn baseline(&self) -> Self {
        Self {
            model_check: None,
            wall_s: None,
            ..self.clone()
        }
    }
}

/// Reads one record per line; blank lines are skipped, and a run twice is refused.
pub fn read(path: &Path, what: &'static str) -> Result<Vec<Record>, Error> {
    read_from(
        fs::File::open(path).map_err(|source| Error::ReadFile {
            what,
            path: path.to_path_buf(),
            source,
        })?,
        path,
        what,
    )
}

/// Decodes records from a reader; `path` identifies the input in diagnostics only.
pub fn read_from(
    mut reader: impl Read,
    path: &Path,
    what: &'static str,
) -> Result<Vec<Record>, Error> {
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .map_err(|source| Error::ReadFile {
            what,
            path: path.to_path_buf(),
            source,
        })?;
    let mut seen = BTreeSet::new();
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Record = serde_json::from_str(line).map_err(|source| Error::Decode {
            what,
            path: path.to_path_buf(),
            line: index + 1,
            source,
        })?;
        if !seen.insert((record.scenario.clone(), record.seed)) {
            return Err(Error::DuplicateRun {
                what,
                scenario: record.scenario,
                seed: record.seed,
            });
        }
        records.push(record);
    }
    Ok(records)
}

/// Writes one record per line, replacing `path` only once every record is written.
pub fn write(path: &Path, records: &[Record]) -> Result<(), Error> {
    write_lines(path, records.iter())
}

/// Writes the records as a baseline file keeps them ([`Record::baseline`]), replacing `path` only once every
/// record is written.
pub fn write_baseline(path: &Path, records: &[Record]) -> Result<(), Error> {
    write_lines(path, records.iter().map(Record::baseline))
}

/// Encodes complete records as JSON Lines without opening files.
pub fn encode(writer: impl io::Write, records: &[Record]) -> io::Result<()> {
    encode_lines(writer, records.iter())
}

/// Encodes the deterministic baseline fields as JSON Lines.
pub fn encode_baseline(writer: impl io::Write, records: &[Record]) -> io::Result<()> {
    encode_lines(writer, records.iter().map(Record::baseline))
}

fn encode_lines(
    mut writer: impl io::Write,
    records: impl Iterator<Item = impl Serialize>,
) -> io::Result<()> {
    for record in records {
        serde_json::to_writer(&mut writer, &record)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()
}

fn write_lines(path: &Path, records: impl Iterator<Item = impl Serialize>) -> Result<(), Error> {
    let failed = |source| Error::WriteFile {
        what: "write records",
        path: path.to_path_buf(),
        source,
    };
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(failed)?;
    {
        let mut out = BufWriter::new(file.as_file_mut());
        encode_lines(&mut out, records).map_err(failed)?;
    }
    file.persist(path).map_err(|error| failed(error.error))?;
    Ok(())
}
