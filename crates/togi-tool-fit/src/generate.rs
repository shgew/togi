//! The target-fit ensemble: the all-facts fit and its whole-trial bootstrap refits, each checked against the
//! original extract and written as a machine file.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use togi_sim::{MachineDefinition, Model};
use togi_tool_modelcheck::{Check, Checker, Group, Status, report};
use togi_tool_trialfacts::{TrialFact, read};

use crate::encode::encode_machine;
use crate::error::Error;
use crate::evidence::{bootstrap, decisive};
use crate::forward::{forward_check, report_forward};
use crate::output::{DiskOutput, MachineOutput};
use crate::paths::facts_reference;
use crate::search::{fit, fit_from};
use crate::signals::report_signals;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ensemble {
    /// Refit `n` resamples with the stream seeded `seed + n`.
    pub seed: u64,
    /// How many whole-trial bootstrap refits follow the all-facts fit.
    pub bootstrap: usize,
}

/// One member of the ensemble as written.
#[derive(Clone, Debug)]
pub struct Fitted {
    pub sample: Vec<TrialFact>,
    pub definition: MachineDefinition,
    pub loss: f64,
    /// The groups the unconstrained refit flagged, which the written refit was restarted to respect.
    pub constrained: Vec<Group>,
    /// The groups the written machine still flags.
    pub flagged: Vec<Group>,
    /// The model check of the encoded machine as read back from its output adapter.
    pub check: Check,
}

#[derive(Debug)]
pub struct Generated {
    pub fits: Vec<Fitted>,
    /// Wall time of the forward-chained check, which ran alongside the fits.
    pub forward_elapsed: Duration,
}

struct Draft {
    sample: Vec<TrialFact>,
    definition: MachineDefinition,
    loss: f64,
    constrained: Vec<Group>,
    flagged: Vec<Group>,
}

fn first_error<T>(results: Vec<Result<T, Error>>) -> Result<Vec<T>, Error> {
    results.into_iter().collect()
}

/// Fits the ensemble to the extract at `facts` and writes `target-fit-N.json` to `out_dir`, then runs the
/// forward-chained check; `report` gets the whole report once every file is written. The files and the
/// report do not depend on the number of threads the ambient rayon pool has.
pub fn generate(
    facts: &Path,
    out_dir: &Path,
    ensemble: Ensemble,
    report_to: &mut impl io::Write,
) -> Result<Generated, Error> {
    let records = read(facts)?;
    generate_records(
        &records,
        facts,
        out_dir,
        ensemble,
        report_to,
        &mut DiskOutput,
    )
}

/// Fits decoded facts and publishes machines through `output`, checking their encoded readback.
pub fn generate_records(
    records: &[TrialFact],
    facts: &Path,
    out_dir: &Path,
    ensemble: Ensemble,
    report_to: &mut impl io::Write,
    output: &mut impl MachineOutput,
) -> Result<Generated, Error> {
    let trials = decisive(records)?;
    let reference = facts_reference(out_dir, facts)?;
    output.prepare(out_dir)?;
    let (written, forward) = rayon::join(
        || {
            write_ensemble(
                records, &trials, facts, out_dir, &reference, ensemble, output,
            )
        },
        || {
            let started = Instant::now();
            let check = forward_check(&trials, 0);
            (check, started.elapsed())
        },
    );
    let (fits, text) = written?;
    report_to.write_all(&text)?;
    let (check, forward_elapsed) = forward;
    report_forward(report_to, &check?, 0)?;
    Ok(Generated {
        fits,
        forward_elapsed,
    })
}

fn member_path(out_dir: &Path, n: usize) -> PathBuf {
    out_dir.join(format!("target-fit-{n}.json"))
}

fn write_ensemble(
    records: &[TrialFact],
    trials: &[TrialFact],
    facts: &Path,
    out_dir: &Path,
    reference: &str,
    ensemble: Ensemble,
    output: &mut impl MachineOutput,
) -> Result<(Vec<Fitted>, Vec<u8>), Error> {
    let drafts: Vec<Result<Draft, Error>> = (0..=ensemble.bootstrap)
        .into_par_iter()
        .map(|n| {
            let sample = if n == 0 {
                trials.to_vec()
            } else {
                bootstrap(
                    trials,
                    ensemble.seed.wrapping_add(u64::try_from(n).unwrap_or(0)),
                )
            };
            let (definition, loss) = fit(&sample)?;
            Ok(Draft {
                sample,
                definition,
                loss,
                constrained: Vec::new(),
                flagged: Vec::new(),
            })
        })
        .collect();
    let drafts = first_error(drafts)?;
    let base = drafts[0].definition.clone();
    let base_model = Model::new(&base, 0)?;
    let checker = Checker::new(&base_model, records)?;
    let base_accepted = checker.accepts(&base_model)?;
    let refined: Vec<Result<Draft, Error>> = drafts
        .into_par_iter()
        .enumerate()
        .map(|(n, mut draft)| {
            let path = member_path(out_dir, n);
            let model = Model::new(&draft.definition, 0)?;
            let mut check = checker.check(&path, facts, &model)?;
            if n > 0 && base_accepted && check.status == Status::Flagged {
                draft.constrained = check.flagged().cloned().collect();
                let (definition, loss) = fit_from(&draft.sample, Some(&base), Some(&checker))?;
                draft.definition = definition;
                draft.loss = loss;
                let model = Model::new(&draft.definition, 0)?;
                check = checker.check(&path, facts, &model)?;
            }
            draft.flagged = check.flagged().cloned().collect();
            Ok(draft)
        })
        .collect();
    let mut text = Vec::new();
    let mut fits = Vec::new();
    for (n, draft) in first_error(refined)?.into_iter().enumerate() {
        let path = member_path(out_dir, n);
        for group in &draft.constrained {
            writeln!(
                text,
                "Refit {n} constraint: {group} unconstrained_interval=[{},{}] mean_p={:.6e}",
                group.interval.low, group.interval.high, group.mean_p
            )?;
        }
        if n > 0 && !base_accepted && !draft.flagged.is_empty() {
            writeln!(
                text,
                "Refit {n} written unconstrained: the all-facts fit fails the model check, so there is no passing fit to restart from"
            )?;
        }
        let mut definition = draft.definition;
        definition.facts = Some(reference.to_owned());
        let content = encode_machine(
            &definition,
            n,
            ensemble.seed,
            draft.sample.len(),
            draft.loss,
            &draft.constrained,
            &draft.flagged,
        )?;
        output.write(&path, &content, "write fitted machine")?;
        let loaded = output.load(&path)?;
        let check = checker.check(&path, facts, &Model::new(&loaded, 0)?)?;
        writeln!(
            text,
            "{}: {} trials; negative log likelihood {:.6}",
            path.display(),
            draft.sample.len(),
            draft.loss
        )?;
        for (j, joint) in definition.joint.iter().enumerate() {
            let half = (definition.cores / 2).max(1);
            let ccd = joint.members.keys().next().map_or(0, |core| core / half);
            let members: Vec<String> = joint
                .members
                .iter()
                .map(|(core, offset)| format!("{core}:{offset}"))
                .collect();
            writeln!(
                text,
                "  CCD{ccd} joint{j}: members={} rate={}/s after={}s",
                members.join(","),
                joint.rate.unwrap_or(0.0),
                joint.after_s
            )?;
        }
        report_signals(&mut text, &definition.model)?;
        fits.push(Fitted {
            sample: draft.sample,
            definition,
            loss: draft.loss,
            constrained: draft.constrained,
            flagged: draft.flagged,
            check,
        });
    }
    let checks: Vec<Check> = fits.iter().map(|fitted| fitted.check.clone()).collect();
    report(&mut text, &checks)?;
    Ok((fits, text))
}
