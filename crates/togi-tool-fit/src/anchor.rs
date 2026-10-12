//! The in-sample shared-voltage anchor: one machine file fitted to every decisive trial and checked against
//! the whole extract before it is written.

use std::fs;
use std::io;
use std::path::Path;

use togi_machine::Regime;
use togi_sim::{MachineDefinition, Model};
use togi_tool_modelcheck::{Status, check_records};
use togi_tool_trialfacts::{TrialFact, read};

use crate::encode::encode_shared_voltage_anchor;
use crate::error::Error;
use crate::evidence::{bios_context_def, decisive, is_failure, load_of, regime_of, trial_class};
use crate::paths::facts_reference;
use crate::signals::report_signals;
use crate::voltage::fit_shared_voltage;

const CORES: usize = 16;

pub fn shared_voltage_anchor(
    facts: &Path,
    out_dir: &Path,
    report: &mut impl io::Write,
) -> Result<(), Error> {
    anchor_with(facts, out_dir, report, fit_shared_voltage)
}

pub(crate) fn anchor_with(
    facts: &Path,
    out_dir: &Path,
    report: &mut impl io::Write,
    fitter: impl FnOnce(&[TrialFact]) -> Result<(MachineDefinition, f64), Error>,
) -> Result<(), Error> {
    let records = read(facts)?;
    let trials = decisive(&records)?;
    for fact in &trials {
        let cores = fact.profile.as_slice().len();
        if cores != CORES {
            return Err(Error::NeedsSixteenCores {
                session: fact.session,
                seq: fact.seq,
                cores,
            });
        }
    }
    let Some(context) = trials[0].context.as_ref() else {
        return Err(Error::NeedsContext);
    };
    let (mut cfg, loss) = fitter(&trials)?;
    if cfg.cores != 16 || cfg.shared_voltage.is_none() {
        return Err(Error::NotSharedVoltage);
    }
    cfg.bios_context = Some(bios_context_def(context));
    cfg.facts = Some(facts_reference(out_dir, facts)?);
    let path = out_dir.join("target-shared-voltage.json");
    let model = Model::new(&cfg, 0)?;
    let check = check_records(&path, facts, &model, &records)?;
    writeln!(
        report,
        "IN-SAMPLE shared-voltage fit; NOT forward-validated. The 2026-10-04 #306 R7 forward bar failed."
    )?;
    if let Some(v) = &cfg.shared_voltage {
        writeln!(
            report,
            "Shared-voltage candidate (record only): rate={}/s margin_v={} background_rate={}/s",
            v.rate, v.margin_v, v.background_rate
        )?;
    }
    report_signals(report, &cfg.model)?;
    writeln!(
        report,
        "\nModel check (unchanged 99% binomial intervals; full original extract)"
    )?;
    writeln!(
        report,
        "{}: {} ({} eligible groups; {} idle failures without trial exposure)",
        path.display(),
        check.status,
        check.groups.len(),
        check.idle_failures
    )?;
    for group in check.flagged() {
        writeln!(
            report,
            "  {group} interval=[{},{}] mean_p={}",
            group.interval.low, group.interval.high, group.mean_p
        )?;
    }
    let (mut r7_loss, mut predicted) = (0.0, 0.0);
    let (mut r7_trials, mut observed) = (0_usize, 0_usize);
    for fact in &trials {
        let Some(class) = trial_class(fact) else {
            continue;
        };
        if regime_of(fact) != Regime::R7 {
            continue;
        }
        let p = model.failure_probability(fact.profile.as_slice(), &load_of(&class))?;
        r7_trials += 1;
        predicted += p;
        if is_failure(fact) {
            observed += 1;
            r7_loss -= p.ln();
        } else {
            r7_loss -= (-p).ln_1p();
        }
    }
    let total = f64::from(u32::try_from(trials.len()).unwrap_or(u32::MAX));
    writeln!(
        report,
        "All trials in-sample (record only): trials={} raw total in-sample log loss {loss} log_loss/trial={}",
        trials.len(),
        loss / total
    )?;
    let r7_mean = if r7_trials > 0 {
        r7_loss / f64::from(u32::try_from(r7_trials).unwrap_or(u32::MAX))
    } else {
        0.0
    };
    writeln!(
        report,
        "R7 in-sample (record only): trials={r7_trials} observed={observed} predicted={predicted} raw_log_loss={r7_loss} log_loss/trial={r7_mean}"
    )?;
    writeln!(
        report,
        "Unsupported y-cruncher R7 thresholds: per core, max of mprime AVX2 and AVX-512; no hardware evidence."
    )?;
    if check.status != Status::Ok {
        return Err(Error::AnchorFlagged);
    }
    let content = encode_shared_voltage_anchor(&cfg, trials.len(), loss)?;
    fs::create_dir_all(out_dir).map_err(|source| Error::Io {
        action: "create output directory",
        path: out_dir.to_path_buf(),
        source,
    })?;
    fs::write(&path, content).map_err(|source| Error::Io {
        action: "write shared-voltage anchor",
        path: path.clone(),
        source,
    })?;
    writeln!(
        report,
        "Wrote IN-SAMPLE anchor: {} (NOT forward-validated)",
        path.display()
    )?;
    Ok(())
}
