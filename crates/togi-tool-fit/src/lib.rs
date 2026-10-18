//! `togi-tool-fit`: fits simulated machines to the real trial facts and checks the fit forward on later
//! sessions. A development program: it never ships.
//!
//! * [`generate_records`] fits the target-machine ensemble: the all-facts fit and its whole-trial bootstrap
//!   refits, each checked against the original extract with the model check and written as a machine file.
//! * [`forward_check`] fits each session's earlier sessions and scores the session, writing no machine file;
//!   [`records`] keeps its every value and [`compare`] pairs a candidate's records with a reference's.
//! * [`shared_voltage_anchor_records`] fits the in-sample 16-core shared-voltage anchor.
//! * [`artifact`] defines the original research artifact, writes it and keeps later sessions out of a run
//!   that does not name the current extract.
//!
//! Every search scores its candidates on rayon's threads, each into its own slot, and folds the slots in
//! candidate order with a strict improvement test; randomness is seeded per fit. The machine files, the
//! report and the forward check are therefore byte for byte the same on a pool of any size.
//!
//! # Fitting objective and search
//!
//! All-facts, whole-trial bootstrap and forward-prefix fits use one path: Bernoulli negative log
//! likelihood plus shared-voltage threshold shrinkage, a margin prior and named-core attribution.
//! Reports show raw Bernoulli loss, excluding priors and attribution. Predictions use
//! `togi_sim::Model::failure_probability` over intended, not measured, trial durations. Equal
//! profile/class observations are aggregated without losing counts; bootstrap trials retain their
//! request/clock telemetry and named-core evidence.
//!
//! Bounded coordinate search fits alone/together limits, workload overrides, hazard shape, supported
//! flat and idle mechanisms, and shared-voltage parameters. It finds a local optimum, not a guaranteed
//! global optimum. Coupled sweeps shift active integer limits by `delta` and scale the past-limit rate
//! by `growth^(-delta)`, preserving already-past-limit hazards until a profile crosses a limit.
//! Unsupported -50 limits stay fixed; the same bounds and original-evidence constraints apply.
//!
//! Positive past-limit rates range from `1e-8` to `0.5` failures/s, near-limit rates from `1e-8` to
//! `0.001`/s, flat rates from `1e-10` to `0.001`/s, growth from `1.05` to `8`, and limits from -50 to 1.
//! Zero is also considered for each rate. No R7 joints or CCD residuals are fitted; the simulator
//! retains their legacy file formats.
//!
//! # Shared-voltage fit
//!
//! Multi-core R7 uses a shared rail instead of per-core limits, joints or CCD residuals. Loaded
//! requests combine a per-core/workload base, count coefficient and clock coefficient. Required
//! voltage combines a threshold with its own clock coefficient times `(CCD MHz-reference MHz)/100`.
//! The margin against the highest loaded request across both CCDs determines the smooth hazard.
//! Request/clock measurements and named-core failure attribution constrain the fit. Threshold
//! shrinkage is 10 mV within CCD/workload; the lognormal margin prior is centered at 6 mV with log-2
//! width. One augmented objective serves every coordinate and sweep acceptance.
//!
//! A separate CO-independent platform background rate applies once per multi-core R7 trial,
//! independently of profile, workload and load size, including CO 0. It produces unattributed
//! crashes. Legacy flat hazards are suppressed in shared R7 and cannot supply this mechanism.
//!
//! | Parameter | Bounds |
//! |---|---|
//! | Request count coefficient | 0.0025–0.005 V/count |
//! | Request and required-voltage clock coefficients | 0.005–0.06 V/100 MHz |
//! | Shared per-core rate | `1e-5`–0.03 failures/s |
//! | Platform background rate | 0–0.001 failures/s |
//! | Smooth margin | 0.001–0.03 V |
//! | Required-voltage thresholds | 0.8–1.5 V |
//! | Per-core/workload request bases | 0.9–1.5 V |
//! | Clock gain per unloaded core | 0–60 MHz/core |
//! | Package and CCD-balance clock responses | 0–30 MHz/W |
//! | Full-load CCD clock intercepts | 4000–6000 MHz |
//!
//! The request reference is 5240 MHz. Power and thermal limits are fixed at 192 W; workload power is
//! 13 W/core for mprime AVX2 and 15 W/core for AVX-512 and y-cruncher, plus 0.12 W per offset count
//! relative to -35. Full-load clocks use measured medians; unsupported ones retain priors.
//! Unsupported y-cruncher R7 uses each core's maximum AVX2/AVX-512 threshold, not a validated
//! hardware threshold.
//!
//! # Identifiability and signals
//!
//! Unsupported or all-passing boundaries stay at -50; no failures does not prove that boundary.
//! Sparse failures cannot resolve separate hazard shapes per core/regime. Unsupported workload
//! overrides remain absent and flat rates remain zero without support. Onset and delays remain zero:
//! binary outcomes do not identify failure-time shapes. Crash-MCE probability, bank attribution and
//! reset kinds retain simulator defaults, outside the binary likelihood. Idle facts have no exposure
//! denominator; idle limits require decisive trials with nonzero unloaded-core offsets.
//!
//! Failure-signal counts set pooled `model.signals` and per-regime `model.regime_signals`. Unseen
//! signals get no weight; regimes without failures use the pooled counts. An uncorrected MCE counts
//! as a crash; the simulator's `crash_mce` controls MCE production. Missing signals are not counted,
//! and a sample without recorded failure signals retains simulator defaults. Named-core attribution
//! affects fitting, while forward and model checks score binary outcomes only.
//!
//! # Bootstrap constraints and reproducibility
//!
//! A bootstrap sample is fitted unconstrained first. If its original-extract model check flags a
//! group and the all-facts fit passes, restart from that passing fit and minimize the same resampled
//! augmented objective, accepting only moves within every original group's unchanged 99% interval.
//! Samples are never redrawn or seeds tuned. This truncates the ensemble's spread: it is not an
//! unconstrained bootstrap confidence interval. If the all-facts fit fails, write flagged members
//! unconstrained instead; a flagged member cannot support a target-machine claim.
//!
//! One Rayon pool bounds independent fits, candidate scoring, constrained restarts and prefix fits.
//! Scores fold in candidate order. Refit `n` resamples with `StdRng` seeded `SEED+n`.
//! Files have stable order and full-precision parameters, no timestamps and omitted default fields.
//! Provenance and model-check diagnostics live in `notes`; BIOS context is preserved as supplied.
//! Fixed inputs and seed reproduce bytes on one host regardless of worker count. Cross-architecture
//! last-digit equality is unmeasured; compare decoded values. Elapsed report times are not
//! deterministic; `mean_p` uses scientific notation. The committed machines remain historical
//! Go-generated output until deliberate regeneration; this optimizer is not expected to reproduce
//! their parameters, bootstrap streams or losses.

mod anchor;
pub mod artifact;
pub mod compare;
mod encode;
mod error;
mod evidence;
mod forward;
mod generate;
mod likelihood;
mod output;
mod paths;
pub mod records;
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
    ForwardCheck, ForwardCounts, ForwardRow, ForwardScore, R7Ccd, R7Rows, TrialScore,
    forward_check, reference_check, report_forward, report_reference,
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
    /// Empty scores no reference. With `records`, also write the check's full-precision
    /// [`records`] to that new file.
    Forward {
        seal: usize,
        reference: Vec<PathBuf>,
        records: Option<PathBuf>,
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
            Mode::Forward {
                seal,
                reference,
                records,
            } => {
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
                let records = records
                    .as_deref()
                    .map(records::RecordsFile::create)
                    .transpose()?;
                let forward = Instant::now();
                let check = forward_check(trials.iter().copied(), *seal)?;
                let elapsed = forward.elapsed();
                report_forward(report, &check, *seal)?;
                if let Some(records) = records {
                    records.write(&extract, *seal, &check)?;
                }
                if !members.is_empty() {
                    let reference = reference_check(trials.iter().copied(), *seal, &members)?;
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
/// original research artifact whether the run's forward check scores its historical confirmation session.
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
    let scored = match mode {
        Mode::Forward { seal, .. } => *seal == 0,
        Mode::Ensemble { .. } => true,
        Mode::SharedVoltage { .. } | Mode::WriteArtifact { .. } => return Ok(()),
    };
    if !extract.is_artifact() {
        return Ok(());
    }
    let status = if scored {
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
