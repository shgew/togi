// Draws against the model's own probabilities, and what draws must not depend on.
// Go: TestCCDLoadedHazardAndDraws (empirical share), TestSharedVoltageDrawsAndScores (empirical share, no MCE),
// TestSharedVoltageBackgroundDraws (the exact draw time is the inverse-of-exposure property in probability.rs),
// TestSharedVoltageBackgroundReplayBypass, TestSharedVoltageSamplesAndDeterminism (same seed, same draw).
// Each test runs a few thousand seeded trials and compares the failure share with `failure_probability`
// within five binomial standard deviations.

use std::time::Duration;
use togi_machine::{Regime, Signal, TrialSpec};
use togi_sim::definition::{CcdDef, JointDef, Weights};
use togi_sim::replay::{Replay, ReplayFact, ReplayOutcome};
use togi_sim::{MachineDefinition, Model, RunSettings};

use crate::common::{
    Rig, basic, committed_machine, cores, crashed, flat_cores, load_of, machine, model, offsets,
    r7_workload, spec,
};
use crate::shared_voltage::tuned_rail;

fn failed(waited: &crate::common::Waited) -> bool {
    crashed(waited) || waited.as_ref().is_ok_and(|result| result.failure.is_some())
}

fn rig_with_seed(model: &Model, seed: u64) -> Rig {
    Rig::with(
        model.clone(),
        RunSettings {
            seed,
            ..RunSettings::default()
        },
    )
}

fn within(measured: f64, expected: f64, trials: u32) {
    let sigma = (expected * (1.0 - expected) / f64::from(trials)).sqrt();
    assert!(
        (measured - expected).abs() <= 5.0 * sigma + 1e-9,
        "measured {measured}, expected {expected} ± {}",
        5.0 * sigma
    );
}

/// The share of `trials` seeds in which `spec` fails after `setup`.
fn failure_share(model: &Model, trials: u32, spec: &TrialSpec, setup: impl Fn(&mut Rig)) -> f64 {
    let mut failures = 0u32;
    for seed in 0..u64::from(trials) {
        let mut rig = rig_with_seed(model, seed);
        setup(&mut rig);
        if failed(&rig.run(spec.clone()).0) {
            failures += 1;
        }
    }
    f64::from(failures) / f64::from(trials)
}

fn expected(model: &Model, profile: &[i64], spec: &TrialSpec) -> f64 {
    model
        .failure_probability(&offsets(profile), &load_of(spec))
        .expect("a matching shape")
}

#[test]
fn ccd_failures_follow_the_integrated_probability() {
    let mut definition = machine(4, flat_cores(4, -50, -50));
    definition.model.signals = Weights::from([(Signal::Crash, 1.0)]);
    definition.ccd = Some(CcdDef {
        log_rate: 0.002f64.ln(),
        slope: 0.1,
        effect: [0.0, 1.0],
    });
    let model = model(&definition);
    let spec = basic(Regime::R7, &[0, 1], Duration::from_secs(90));
    let p = expected(&model, &[-25; 4], &spec);
    assert!((p - (-(-0.002f64 * 90.0).exp_m1())).abs() < 1e-12);
    let share = failure_share(&model, 3000, &spec, |rig| {
        rig.set_all(-25).expect("set");
    });
    within(share, p, 3000);
}

#[test]
fn a_delayed_joint_with_an_onset_boost_fails_as_often_as_its_exposure_says() {
    let mut definition = machine(2, flat_cores(2, -50, -50));
    definition.model.signals = Weights::from([(Signal::Crash, 1.0)]);
    definition.model.onset_s = 20.0;
    definition.model.onset_boost = 2.0;
    definition.joint = vec![JointDef {
        members: [(0, -10)].into(),
        rate: Some(0.01),
        after_s: 10.0,
        ..JointDef::default()
    }];
    let model = model(&definition);
    let spec = basic(Regime::R1, &[0], Duration::from_secs(60));
    let p = expected(&model, &[-10, 0], &spec);
    within(
        failure_share(&model, 3000, &spec, |rig| rig.set(0, -10).expect("set")),
        p,
        3000,
    );
}

#[test]
fn a_loaded_cores_flat_rate_matches_its_probability() {
    let mut definition = machine(2, flat_cores(2, -50, -50));
    definition.core[0].flat = 0.01;
    definition.model.signals = Weights::from([(Signal::Crash, 1.0)]);
    let model = model(&definition);
    let spec = basic(Regime::R1, &[0], Duration::from_secs(90));
    let p = expected(&model, &[-5, 0], &spec);
    within(
        failure_share(&model, 3000, &spec, |rig| rig.set(0, -5).expect("set")),
        p,
        3000,
    );
}

fn voltage_trial() -> TrialSpec {
    spec(
        "0001",
        Regime::R7,
        togi_machine::workload_by_id(r7_workload(0)).expect("an R7 workload"),
        &[0, 1],
        Duration::from_secs(120),
    )
}

#[test]
fn the_shared_rail_fails_as_its_probability_says_and_names_no_machine_check() {
    let mut definition = tuned_rail(committed_machine("shared-voltage.json"));
    definition.ccd = Some(CcdDef {
        log_rate: 10.0,
        ..CcdDef::default()
    });
    definition.joint = vec![JointDef {
        members: [(0, 0)].into(),
        regimes: vec![Regime::R7],
        rate: Some(100.0),
        ..JointDef::default()
    }];
    definition.model.crash_mce = 1.0;
    definition.model.signals = Weights::from([(Signal::Stall, 1.0)]);
    let model = model(&definition);
    let spec = voltage_trial();
    let p = expected(&model, &[0; 16], &spec);
    let trials = 2000u32;
    let mut failures = 0u32;
    for seed in 0..u64::from(trials) {
        let mut rig = rig_with_seed(&model, seed);
        let waited = rig.run(spec.clone()).0;
        if crashed(&waited) {
            failures += 1;
            rig.sim.reboot();
            let boot = rig.boot();
            assert!(
                rig.mces(&boot).is_empty(),
                "a rail crash fabricated machine-check evidence"
            );
        } else {
            assert!(waited.expect("a pass").failure.is_none());
        }
    }
    within(f64::from(failures) / f64::from(trials), p, trials);
}

fn background_machine(rate: f64) -> MachineDefinition {
    let mut definition = committed_machine("shared-voltage.json");
    definition.core = flat_cores(16, -50, -50);
    let rail = definition.shared_voltage.as_mut().expect("a rail");
    rail.background_rate = rate;
    for workload in rail.workload.values_mut() {
        for core in &mut workload.core {
            core.base_v = 100.0;
        }
    }
    definition.model.onset_boost = 2.0;
    definition.model.onset_s = 10.0;
    definition.model.crash_mce = 1.0;
    definition.model.signals = Weights::from([(Signal::ComputationError, 1.0)]);
    definition
}

#[test]
fn background_failures_are_unattributed_crashes_and_match_their_probability() {
    let model = model(&background_machine(0.001));
    let spec = voltage_trial();
    let p = expected(&model, &[0; 16], &spec);
    let trials = 3000u32;
    let mut failures = 0u32;
    for seed in 0..u64::from(trials) {
        let mut rig = rig_with_seed(&model, seed);
        let waited = rig.run(spec.clone()).0;
        if !crashed(&waited) {
            continue;
        }
        failures += 1;
        if failures <= 20 {
            let samples: Vec<_> = rig
                .machine
                .trials
                .samples("0001")
                .collect::<Result<_, _>>()
                .unwrap();
            if let Some(last) = samples.last() {
                for core in cores(&[0, 1]) {
                    assert_eq!(
                        last.worker_cpu_ms.get(core),
                        Some(last.elapsed_ms),
                        "no worker stalls on a platform failure"
                    );
                }
            }
            rig.sim.reboot();
            let boot = rig.boot();
            assert!(
                rig.mces(&boot).is_empty(),
                "the background fabricated an MCE"
            );
        }
    }
    within(f64::from(failures) / f64::from(trials), p, trials);
}

#[test]
fn a_replayed_pass_overrides_the_background() {
    let model = model(&background_machine(100.0));
    let spec = voltage_trial();
    let replay = Replay::new([ReplayFact {
        context: model.bios_context().clone(),
        regime: Regime::R7,
        workload: spec.workload.as_str().to_owned(),
        cores: cores(&[0, 1]),
        duration: spec.duration,
        profile: offsets(&[0; 16]),
        outcome: ReplayOutcome::Pass,
    }])
    .expect("valid facts");
    let mut rig = Rig::with(
        model,
        RunSettings {
            seed: 1,
            replay: Some(replay),
            ..RunSettings::default()
        },
    );
    let result = rig.pass(spec.clone());
    assert!(result.failure.is_none());
    assert_eq!(result.ran, spec.duration);
}

#[test]
fn the_same_seed_draws_the_same_failures_and_telemetry_changes_none() {
    // Failures must be likely, or a telemetry that perturbed them would go unseen.
    let model = model(&background_machine(0.005));
    let p = expected(&model, &[0; 16], &voltage_trial());
    assert!((0.3..=0.9).contains(&p), "{p}");
    let run = |seed: u64, with_samples: bool| {
        let dir = std::rc::Rc::new(crate::common::MemorySamples::default());
        let mut rig = rig_with_seed(&model, seed);
        if with_samples {
            rig.sim.set_sample_storage(dir.clone());
        }
        let mut out = Vec::new();
        let mut failures = 0usize;
        for index in 0..40usize {
            let mut trial = voltage_trial();
            trial.index = index;
            trial.id = format!("{index:04}");
            let waited = rig.run(trial).0;
            let lanes = rig.machine.trials.samples(&format!("{index:04}")).count();
            // The outcome, its exposure and when it ended.
            out.push(format!("{waited:?} {lanes} {:?}", rig.sim.now()));
            if failed(&waited) {
                failures += 1;
            }
            if crashed(&waited) {
                rig.sim.reboot();
            }
            let _ = rig.machine.host.preflight();
            let _ = rig
                .machine
                .trials
                .sweep(&togi_machine::CancellationToken::new());
        }
        (out, rig.sim.now(), failures)
    };
    let plain = run(5, false);
    assert!(
        (1..40).contains(&plain.2),
        "the fixed seed must mix failures and passes: {} failures",
        plain.2
    );
    assert_eq!(plain, run(5, false));
    assert_eq!(plain, run(5, true), "telemetry consumes no failure draw");
    assert_ne!(plain.0, run(6, false).0);
}
