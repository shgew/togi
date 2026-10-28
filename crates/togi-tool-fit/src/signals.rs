use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;

use togi_facts::Decisive;
use togi_machine::{LEGACY_REGIMES, Regime, Signal};
use togi_sim::MachineDefinition;
use togi_sim::definition::{ModelDef, Weights};
use togi_tool_trialfacts::TrialFact;

use crate::evidence::regime_of;

/// Sets the failure signal mix to the counts of the records' failure signals: pooled over every regime in
/// `model.signals`, which regimes without failures draw from, and per regime with failures in
/// `model.regime_signals`. Signal weights are relative, so counts are their maximum-likelihood estimate. An
/// uncorrected MCE counts as a crash, which the simulator draws as a crash leaving an MCE with probability
/// `crash_mce`. Without any failure the mix is the simulator's default weights and no `regime_signals`,
/// whatever the starting model held.
pub(crate) fn fit_signals(cfg: &mut MachineDefinition, records: &[&TrialFact]) {
    let mut pooled: BTreeMap<Signal, f64> = BTreeMap::new();
    let mut regimes: BTreeMap<Regime, BTreeMap<Signal, f64>> = BTreeMap::new();
    for fact in records {
        let Decisive::Failure { signal, .. } = fact.outcome else {
            continue;
        };
        let signal = if signal == Signal::UncorrectedMce {
            Signal::Crash
        } else {
            signal
        };
        *pooled.entry(signal).or_default() += 1.0;
        *regimes
            .entry(regime_of(fact))
            .or_default()
            .entry(signal)
            .or_default() += 1.0;
    }
    if pooled.is_empty() {
        let defaults = ModelDef::default();
        cfg.model.signals = defaults.signals;
        cfg.model.regime_signals = BTreeMap::new();
    } else {
        cfg.model.signals = Weights(pooled);
        cfg.model.regime_signals = regimes.into_iter().map(|(r, w)| (r, Weights(w))).collect();
    }
}

fn failure_count(counts: &Weights<Signal>) -> String {
    let total: f64 = counts.0.values().sum();
    if (total - 1.0).abs() < f64::EPSILON {
        "1 failure".to_owned()
    } else {
        format!("{total} failures")
    }
}

fn signal_counts(counts: &Weights<Signal>) -> String {
    let mut text = String::new();
    for (i, (signal, count)) in counts.0.iter().enumerate() {
        if i > 0 {
            text.push(' ');
        }
        let _ = write!(text, "{signal}={count}");
    }
    text
}

/// Prints the fitted signal mix as failure counts, pooled and per regime.
pub(crate) fn report_signals(out: &mut impl io::Write, model: &ModelDef) -> io::Result<()> {
    if model.regime_signals.is_empty() {
        return writeln!(out, "  signals: no failures; default weights");
    }
    writeln!(
        out,
        "  signals from {}: {}",
        failure_count(&model.signals),
        signal_counts(&model.signals)
    )?;
    for regime in LEGACY_REGIMES {
        match model.regime_signals.get(&regime) {
            Some(counts) => writeln!(
                out,
                "    {regime} from {}: {}",
                failure_count(counts),
                signal_counts(counts)
            )?,
            None => writeln!(out, "    {regime}: no failures; pooled mix")?,
        }
    }
    Ok(())
}

/// Says how many failures a machine's signal weights were counted from.
pub(crate) fn signals_note(model: &ModelDef) -> String {
    if model.regime_signals.is_empty() {
        "Signal mix: no failures; default weights.".to_owned()
    } else {
        format!(
            "Signal mix: counted from {}.",
            failure_count(&model.signals)
        )
    }
}
