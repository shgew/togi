//! `togi-tool-fit`: fits simulated machines to the real trial facts and checks the fit forward on later
//! sessions. A development program: it never ships.
//!
//! * [`generate_records`] fits the target-machine ensemble: the all-facts fit and its whole-trial bootstrap
//!   refits, each checked against the original extract with the model check and written as a machine file.
//! * [`forward_check`] fits each session's earlier sessions and scores the session, writing no machine file.
//! * [`shared_voltage_anchor_records`] fits the in-sample 16-core shared-voltage anchor.
//! * [`artifact`] defines the original research artifact, writes it and keeps later sessions out of a run
//!   that does not name the current extract.
//!
//! Every search scores its candidates on rayon's threads, each into its own slot, and folds the slots in
//! candidate order with a strict improvement test; randomness is seeded per fit. The machine files, the
//! report and the forward check are therefore byte for byte the same on a pool of any size.

mod anchor;
pub mod artifact;
mod encode;
mod error;
mod evidence;
mod forward;
mod generate;
mod likelihood;
mod output;
mod paths;
mod search;
mod signals;
mod voltage;

#[cfg(test)]
mod tests;

use std::io;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use output::DiskOutput;

pub use anchor::shared_voltage_anchor_records;
pub use artifact::{Extract, Sessions};
pub use error::{Error, EvidenceError, Problem};
pub use forward::{
    ForwardCheck, ForwardCounts, ForwardRow, ForwardScore, R7Ccd, R7Rows, forward_check,
    reference_check, report_forward, report_reference,
};
pub use generate::{Ensemble, Fitted, Generated, generate_records};
pub use output::MachineOutput;

/// What one run does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Fit the ensemble into the directory `out`, then run the forward-chained check.
    Ensemble { ensemble: Ensemble, out: PathBuf },
    /// Run only the forward-chained check, leaving the newest `seal` sessions out, and score the machine
    /// files of a committed ensemble, `reference`, on the same held-out sessions without fitting them.
    /// Empty scores no reference.
    Forward {
        seal: usize,
        reference: Vec<PathBuf>,
    },
    /// Fit the in-sample shared-voltage anchor into the directory `out`.
    SharedVoltage { out: PathBuf },
    /// Write the original research artifact from the extract to the new file `out`, fitting nothing.
    WriteArtifact { out: PathBuf },
}

#[derive(Clone, Debug)]
pub struct Options {
    /// The gzip extract of decisive trial facts.
    pub facts: PathBuf,
    /// The sessions the run may read from `facts`.
    pub sessions: Sessions,
    /// Threads to run on; the ambient rayon pool when absent. The results do not depend on it.
    pub jobs: Option<NonZeroUsize>,
    pub mode: Mode,
}

/// Wall times of a run, which are not part of its deterministic report.
#[derive(Clone, Copy, Debug, Default)]
pub struct Elapsed {
    pub total: Duration,
    pub forward: Option<Duration>,
}

/// Runs `options`, writing the report to `report`. Every mode but [`Mode::WriteArtifact`] reads `facts`
/// through [`artifact::load`] and reports the hash of its rows first.
pub fn run(options: &Options, report: &mut (impl io::Write + Send)) -> Result<Elapsed, Error> {
    let started = Instant::now();
    let mut work = || -> Result<Option<Duration>, Error> {
        if let Mode::WriteArtifact { out } = &options.mode {
            let rows = artifact::materialize(&options.facts, out)?;
            writeln!(
                report,
                "Wrote the original research artifact to {}: {rows} rows of {} sessions, sha256 {}",
                out.display(),
                artifact::DEVELOPMENT.len() + 1,
                artifact::SHA256
            )?;
            return Ok(None);
        }
        let extract = artifact::load(&options.facts, options.sessions)?;
        report_extract(report, &extract, &options.mode)?;
        match &options.mode {
            Mode::Ensemble { ensemble, out } => {
                let generated = generate_records(
                    &extract.facts,
                    &options.facts,
                    out,
                    *ensemble,
                    report,
                    &mut DiskOutput,
                )?;
                Ok(Some(generated.forward_elapsed))
            }
            Mode::Forward { seal, reference } => {
                let members = reference
                    .iter()
                    .map(|path| {
                        togi_sim::file::load(path).map_err(|source| Error::Load {
                            path: path.clone(),
                            source,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let trials = evidence::decisive(&extract.facts)?;
                let forward = Instant::now();
                let check = forward_check(&trials, *seal)?;
                let elapsed = forward.elapsed();
                report_forward(report, &check, *seal)?;
                if !members.is_empty() {
                    let reference = reference_check(&trials, *seal, &members)?;
                    report_reference(report, &reference, *seal, members.len())?;
                }
                Ok(Some(elapsed))
            }
            Mode::SharedVoltage { out } => {
                shared_voltage_anchor_records(
                    &extract.facts,
                    &options.facts,
                    out,
                    report,
                    &mut DiskOutput,
                )?;
                Ok(None)
            }
            Mode::WriteArtifact { .. } => Ok(None),
        }
    };
    let forward = match options.jobs {
        Some(jobs) => rayon::ThreadPoolBuilder::new()
            .num_threads(jobs.get())
            .build()?
            .install(work)?,
        None => work()?,
    };
    Ok(Elapsed {
        total: started.elapsed(),
        forward,
    })
}

/// The identity of the rows a run reads, so its raw output names the data it rests on, and for the
/// original research artifact whether a forward check scores its historical confirmation session.
pub(crate) fn report_extract(
    report: &mut impl io::Write,
    extract: &Extract,
    mode: &Mode,
) -> io::Result<()> {
    let artifact = if extract.is_artifact() {
        "the original research artifact"
    } else {
        "not the original research artifact"
    };
    writeln!(
        report,
        "Facts: sha256 {}, {} rows, {} sessions: {artifact}",
        extract.sha256, extract.rows, extract.sessions
    )?;
    match mode {
        Mode::Forward { seal, .. } if extract.is_artifact() => {
            let status = if *seal == 0 {
                "scored, a historical check that is not blind"
            } else {
                "sealed, neither fitted nor scored"
            };
            writeln!(
                report,
                "Historical confirmation {}: {status}",
                artifact::CONFIRMATION
            )
        }
        Mode::Forward { .. }
        | Mode::Ensemble { .. }
        | Mode::SharedVoltage { .. }
        | Mode::WriteArtifact { .. } => Ok(()),
    }
}
