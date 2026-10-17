//! `togi-tool-fit`: fits simulated machines to the real trial facts and checks the fit forward on later
//! sessions. A development program: it never ships.
//!
//! * [`generate`] fits the target-machine ensemble: the all-facts fit and its whole-trial bootstrap refits,
//!   each checked against the original extract with the model check and written as a machine file.
//! * [`forward_check`] fits each session's earlier sessions and scores the session, writing no machine file.
//! * [`shared_voltage_anchor`] fits the in-sample 16-core shared-voltage anchor.
//!
//! Every search scores its candidates on rayon's threads, each into its own slot, and folds the slots in
//! candidate order with a strict improvement test; randomness is seeded per fit. The machine files, the
//! report and the forward check are therefore byte for byte the same on a pool of any size.

mod anchor;
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

pub use anchor::{shared_voltage_anchor, shared_voltage_anchor_records};
pub use error::{Error, EvidenceError, Problem};
pub use forward::{
    ForwardCheck, ForwardCounts, ForwardRow, ForwardScore, R7Ccd, R7Rows, forward_check,
    reference_check, report_forward, report_reference,
};
pub use generate::{Ensemble, Fitted, Generated, generate, generate_records};
pub use output::MachineOutput;

/// What one run does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Fit and write the ensemble, then run the forward-chained check.
    Ensemble(Ensemble),
    /// Run only the forward-chained check, leaving the newest `seal` sessions out.
    Forward { seal: usize },
    /// Fit and write the in-sample shared-voltage anchor.
    SharedVoltage,
}

#[derive(Clone, Debug)]
pub struct Options {
    /// The gzip extract of decisive trial facts.
    pub facts: PathBuf,
    /// The directory machine files are written to.
    pub out: PathBuf,
    /// Threads to run on; the ambient rayon pool when absent. The results do not depend on it.
    pub jobs: Option<NonZeroUsize>,
    /// With [`Mode::Forward`], machine files of a committed ensemble to score on the same held-out
    /// sessions beside the candidate, without fitting them. Empty scores no reference.
    pub reference: Vec<PathBuf>,
    pub mode: Mode,
}

/// Wall times of a run, which are not part of its deterministic report.
#[derive(Clone, Copy, Debug, Default)]
pub struct Elapsed {
    pub total: Duration,
    pub forward: Option<Duration>,
}

/// Runs `options`, writing the report to `report`.
pub fn run(options: &Options, report: &mut (impl io::Write + Send)) -> Result<Elapsed, Error> {
    let started = Instant::now();
    let mut work = || -> Result<Option<Duration>, Error> {
        match options.mode {
            Mode::Ensemble(ensemble) => {
                let generated = generate(&options.facts, &options.out, ensemble, report)?;
                Ok(Some(generated.forward_elapsed))
            }
            Mode::Forward { seal } => {
                let members = options
                    .reference
                    .iter()
                    .map(|path| {
                        togi_sim::file::load(path).map_err(|source| Error::Load {
                            path: path.clone(),
                            source,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let records = togi_tool_trialfacts::read(&options.facts)?;
                let trials = evidence::decisive(&records)?;
                let forward = Instant::now();
                let check = forward_check(&trials, seal)?;
                let elapsed = forward.elapsed();
                report_forward(report, &check, seal)?;
                if !members.is_empty() {
                    let reference = reference_check(&trials, seal, &members)?;
                    report_reference(report, &reference, seal, members.len())?;
                }
                Ok(Some(elapsed))
            }
            Mode::SharedVoltage => {
                shared_voltage_anchor(&options.facts, &options.out, report)?;
                Ok(None)
            }
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
