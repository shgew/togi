use togi_facts::Decisive;
use togi_machine::{Regime, Signal};
use togi_sim::MachineDefinition;
use togi_sim::definition::{ModelDef, Weights};

use super::{fact, failure};
use crate::signals::{fit_signals, report_signals, signals_note};

fn failing(regime: Regime, signal: Signal) -> togi_tool_trialfacts::TrialFact {
    fact(&[0, 0], regime, &[0], 60, failure(signal))
}

fn report(model: &ModelDef) -> String {
    let mut out = Vec::new();
    report_signals(&mut out, model).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn the_signal_report_names_the_failure_counts_pooled_and_per_regime() {
    let mut cfg = MachineDefinition::default();
    fit_signals(
        &mut cfg,
        &[
            failing(Regime::R1, Signal::UnexpectedExit),
            failing(Regime::R7, Signal::Crash),
            failing(Regime::R7, Signal::ComputationError),
        ],
    );
    assert_eq!(
        report(&cfg.model),
        "  signals from 3 failures: computation_error=1 unexpected_exit=1 crash=1
    R1 from 1 failure: unexpected_exit=1
    R2: no failures; pooled mix
    R3: no failures; pooled mix
    R4: no failures; pooled mix
    R5: no failures; pooled mix
    R6: no failures; pooled mix
    R7 from 2 failures: computation_error=1 crash=1
"
    );
    assert_eq!(
        signals_note(&cfg.model),
        "Signal mix: counted from 3 failures."
    );
}

#[test]
fn an_uncorrected_mce_counts_as_a_crash() {
    let mut cfg = MachineDefinition::default();
    fit_signals(&mut cfg, &[failing(Regime::R7, Signal::UncorrectedMce)]);
    assert_eq!(cfg.model.signals, Weights::from([(Signal::Crash, 1.0)]));
    assert_eq!(
        signals_note(&cfg.model),
        "Signal mix: counted from 1 failure."
    );
}

#[test]
fn without_any_failure_the_mix_is_the_default_whatever_the_machine_held() {
    let mut cfg = MachineDefinition::default();
    cfg.model.signals = Weights::from([(Signal::Crash, 3.0), (Signal::ComputationError, 1.0)]);
    cfg.model.regime_signals = [(Regime::R7, Weights::from([(Signal::Crash, 3.0)]))].into();
    fit_signals(
        &mut cfg,
        &[fact(&[0, 0], Regime::R7, &[0], 60, Decisive::Pass)],
    );
    assert_eq!(cfg.model.signals, ModelDef::default().signals);
    assert!(cfg.model.regime_signals.is_empty());
    assert_eq!(
        report(&cfg.model),
        "  signals: no failures; default weights\n"
    );
    assert_eq!(
        signals_note(&cfg.model),
        "Signal mix: no failures; default weights."
    );
}
