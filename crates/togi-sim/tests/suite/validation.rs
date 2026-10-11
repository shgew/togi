// What `Model::new` refuses, and the field path it names.
// Go: TestNewRejects*, TestValidateCoresBound, TestLoadMachineInvalidDefinitions (core table),
// TestNewRejectsInvalidScriptCore, the hazard and joint rejections of TestNewRejectsInvalidHazardsAndSignals,
// TestNewRejectsInvalidModelTopology, TestNewRejectsInvalidModelWeights, TestCCDValidation,
// TestNewPredictorRejects (negative joint rate, member core and offset out of range).

use std::collections::BTreeMap;
use togi_machine::{ResetKind, Signal};
use togi_sim::definition::{CcdDef, JointDef, ScriptDef, Weights};
use togi_sim::{MachineDefinition, Model, Problem};

use crate::common::{core_def, flat_cores, invalid, machine, model};

fn refused(definition: &MachineDefinition, path: &str, problem: &Problem) {
    let error = invalid(definition);
    assert_eq!(
        (error.path.as_str(), &error.problem),
        (path, problem),
        "{error}"
    );
}

fn two_cores() -> MachineDefinition {
    machine(2, flat_cores(2, -10, -10))
}

fn joint(members: &[(i64, i64)]) -> JointDef {
    JointDef {
        members: members.iter().copied().collect(),
        ..JointDef::default()
    }
}

#[test]
fn the_core_count_is_even_from_two_to_sixteen() {
    for cores in [2, 4, 6, 8, 10, 12, 14, 16] {
        assert!(Model::new(&machine(cores, vec![]), 1).is_ok(), "{cores}");
    }
    for cores in [-2, 0, 1, 3, 17, 18, 32] {
        refused(&machine(cores, vec![]), "cores", &Problem::Topology(cores));
    }
}

#[test]
fn topology_is_checked_before_the_limits() {
    refused(
        &machine(18, vec![core_def(0, -10, -10)]),
        "cores",
        &Problem::Topology(18),
    );
}

#[test]
fn the_core_table_names_every_core_exactly_once() {
    let mut definition = two_cores();
    definition.core[1].id = -1;
    refused(
        &definition,
        "core[1].id",
        &Problem::OutOfRange {
            value: -1,
            min: 0,
            max: 1,
        },
    );
    definition.core[1].id = 2;
    refused(
        &definition,
        "core[1].id",
        &Problem::OutOfRange {
            value: 2,
            min: 0,
            max: 1,
        },
    );
    definition.core[1].id = 0;
    refused(&definition, "core[1].id", &Problem::Duplicate);
    definition.core.truncate(1);
    refused(&definition, "core", &Problem::MissingCore(1));
    let mut default_topology = machine(16, vec![core_def(0, -10, -10)]);
    refused(&default_topology, "core", &Problem::MissingCore(1));
    default_topology.core = flat_cores(16, -10, -10);
    default_topology.core.reverse();
    assert!(
        Model::new(&default_topology, 1).is_ok(),
        "core order is free"
    );
}

#[test]
fn limits_idle_and_workload_limits_stay_within_minus_fifty_and_one() {
    let out = |value| Problem::OutOfRange {
        value,
        min: -50,
        max: 1,
    };
    let mut definition = two_cores();
    definition.core[1].alone[3] = 1;
    definition.core[1].together[6] = -50;
    definition.core[1].idle = Some(1);
    definition.core[1].workload = BTreeMap::from([("w".to_owned(), -50)]);
    assert!(Model::new(&definition, 1).is_ok());

    definition.core[1].alone[3] = 2;
    refused(&definition, "core[1].alone[3]", &out(2));
    definition.core[1].alone[3] = -51;
    refused(&definition, "core[1].alone[3]", &out(-51));
    definition.core[1].alone[3] = -10;
    definition.core[1].together[6] = 2;
    refused(&definition, "core[1].together[6]", &out(2));
    definition.core[1].together[6] = -10;
    definition.core[1].idle = Some(-51);
    refused(&definition, "core[1].idle", &out(-51));
    definition.core[1].idle = Some(2);
    refused(&definition, "core[1].idle", &out(2));
    definition.core[1].idle = None;
    definition.core[1].workload = BTreeMap::from([("custom".to_owned(), -51)]);
    refused(&definition, "core[1].workload.custom", &out(-51));
    definition.core[1].workload = BTreeMap::from([("custom".to_owned(), 2)]);
    refused(&definition, "core[1].workload.custom", &out(2));
}

#[test]
fn flat_rates_are_finite_and_nonnegative() {
    let mut definition = two_cores();
    for (flat, problem) in [
        (-1.0, Problem::Negative),
        (f64::NAN, Problem::NotFinite),
        (f64::INFINITY, Problem::NotFinite),
    ] {
        definition.core[1].flat = flat;
        refused(&definition, "core[1].flat", &problem);
    }
}

#[test]
fn bios_and_ranking_have_one_value_per_core() {
    let mut definition = two_cores();
    definition.bios = Some(vec![0]);
    refused(
        &definition,
        "bios",
        &Problem::Length {
            expected: 2,
            found: 1,
        },
    );
    definition.bios = Some(vec![0, -51]);
    refused(
        &definition,
        "bios[1]",
        &Problem::OutOfRange {
            value: -51,
            min: -50,
            max: 0,
        },
    );
    definition.bios = Some(vec![1, 0]);
    refused(
        &definition,
        "bios[0]",
        &Problem::OutOfRange {
            value: 1,
            min: -50,
            max: 0,
        },
    );
    definition.bios = Some(vec![-50, 0]);
    assert_eq!(
        model(&definition)
            .bios()
            .iter()
            .map(|offset| offset.get())
            .collect::<Vec<_>>(),
        [-50, 0]
    );
    definition.ranking = Some(vec![1]);
    refused(
        &definition,
        "ranking",
        &Problem::Length {
            expected: 2,
            found: 1,
        },
    );
}

#[test]
fn scripts_name_a_trial_once_and_a_core_of_the_machine() {
    let script = |trial: &str, core: i64| ScriptDef {
        trial: trial.to_owned(),
        core,
        ..ScriptDef::default()
    };
    for core in [-1, 2] {
        let mut definition = two_cores();
        definition.script = vec![script("0001", core)];
        refused(
            &definition,
            "script[0].core",
            &Problem::OutOfRange {
                value: core,
                min: 0,
                max: 1,
            },
        );
    }
    let mut definition = two_cores();
    definition.script = vec![script("", 0)];
    refused(&definition, "script[0].trial", &Problem::Empty);
    definition.script = vec![script("0001", 0), script("0001", 1)];
    refused(&definition, "script[1].trial", &Problem::Duplicate);
    definition.script = vec![ScriptDef {
        at_s: -1.0,
        ..script("0001", 0)
    }];
    refused(&definition, "script[0].at_s", &Problem::Negative);
}

#[test]
fn joints_need_members_on_real_cores_and_valid_numbers() {
    let mut definition = two_cores();
    definition.joint = vec![joint(&[])];
    refused(&definition, "joint[0].members", &Problem::Empty);
    for core in [-1, 2] {
        definition.joint = vec![joint(&[(core, -10)])];
        refused(
            &definition,
            &format!("joint[0].members.{core}"),
            &Problem::OutOfRange {
                value: core,
                min: 0,
                max: 1,
            },
        );
    }
    definition.joint = vec![joint(&[(0, -5)]), joint(&[(0, -5), (1, 1)])];
    refused(
        &definition,
        "joint[1].members.1",
        &Problem::OutOfRange {
            value: 1,
            min: -50,
            max: 0,
        },
    );
    let mut bad = joint(&[(0, -5)]);
    bad.rate = Some(-1.0);
    definition.joint = vec![bad.clone()];
    refused(&definition, "joint[0].rate", &Problem::Negative);
    bad.rate = Some(f64::NAN);
    definition.joint = vec![bad.clone()];
    refused(&definition, "joint[0].rate", &Problem::NotFinite);
    bad.rate = None;
    bad.after_s = -1.0;
    definition.joint = vec![bad.clone()];
    refused(&definition, "joint[0].after_s", &Problem::Negative);
    bad.after_s = f64::INFINITY;
    definition.joint = vec![bad.clone()];
    refused(&definition, "joint[0].after_s", &Problem::NotFinite);
    bad.after_s = 0.0;
    bad.crash_mce_core = Some(2);
    definition.joint = vec![bad];
    refused(
        &definition,
        "joint[0].crash_mce_core",
        &Problem::OutOfRange {
            value: 2,
            min: 0,
            max: 1,
        },
    );
}

#[test]
fn a_joint_without_a_rate_takes_the_model_rate_and_an_explicit_zero_stays_zero() {
    let mut definition = two_cores();
    definition.model.past_limit_rate = 0.25;
    let mut inherits = joint(&[(0, -5)]);
    let mut silent = joint(&[(1, -5)]);
    silent.rate = Some(0.0);
    inherits.rate = None;
    definition.joint = vec![inherits, silent];
    let model = model(&definition);
    assert_eq!(model.joints()[0].rate, 0.25);
    assert_eq!(model.joints()[1].rate, 0.0);
}

#[test]
fn weights_are_finite_nonnegative_with_a_positive_total() {
    let mut definition = two_cores();
    let mut set = |weights: &[(Signal, f64)]| {
        definition.model.signals = Weights(weights.iter().copied().collect());
        invalid(&definition)
    };
    let error = set(&[(Signal::Stall, -1.0), (Signal::ComputationError, -2.0)]);
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("model.signals.computation_error", Problem::Negative)
    );
    let error = set(&[]);
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("model.signals", Problem::ZeroTotal)
    );
    let error = set(&[(Signal::Crash, 0.0)]);
    assert_eq!(error.problem, Problem::ZeroTotal);
    let error = set(&[(Signal::Crash, f64::INFINITY)]);
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("model.signals.crash", Problem::NotFinite)
    );
    let error = set(&[(Signal::Crash, f64::NAN)]);
    assert_eq!(error.problem, Problem::NotFinite);
    let error = set(&[(Signal::Crash, f64::MAX), (Signal::Stall, f64::MAX)]);
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("model.signals", Problem::NotFinite)
    );

    definition.model.signals = Weights::from([(Signal::Crash, 1.0)]);
    definition.model.regime_signals = BTreeMap::from([(
        togi_machine::Regime::R7,
        Weights::from([(Signal::Crash, 1.0)]),
    )]);
    assert!(Model::new(&definition, 1).is_ok());
    definition
        .model
        .regime_signals
        .insert(togi_machine::Regime::R2, Weights::default());
    refused(&definition, "model.regime_signals.R2", &Problem::ZeroTotal);
}

#[test]
fn reset_weights_are_finite_nonnegative_with_a_positive_total() {
    let mut definition = two_cores();
    definition.model.reset =
        Weights::from([(ResetKind::Watchdog, -1.0), (ResetKind::ThermalTrip, -2.0)]);
    refused(&definition, "model.reset.watchdog", &Problem::Negative);
    definition.model.reset = Weights::default();
    refused(&definition, "model.reset", &Problem::ZeroTotal);
    definition.model.reset = Weights::from([(ResetKind::PowerLoss, 0.0)]);
    refused(&definition, "model.reset", &Problem::ZeroTotal);
    definition.model.reset = Weights::from([(ResetKind::PowerLoss, 3.0)]);
    assert_eq!(model(&definition).reset_mix().total(), 3.0);
}

#[test]
fn model_scalars_are_checked() {
    type Edit = fn(&mut MachineDefinition);
    let cases: [(&str, Problem, Edit); 15] = [
        ("model.past_limit_rate", Problem::Negative, |d| {
            d.model.past_limit_rate = -1.0;
        }),
        ("model.past_limit_rate", Problem::NotFinite, |d| {
            d.model.past_limit_rate = f64::INFINITY;
        }),
        ("model.growth", Problem::NotPositive, |d| {
            d.model.growth = 0.0;
        }),
        ("model.growth", Problem::NotPositive, |d| {
            d.model.growth = -2.0;
        }),
        ("model.growth", Problem::NotFinite, |d| {
            d.model.growth = f64::NAN;
        }),
        ("model.growth", Problem::Overflow, |d| d.model.growth = 1e10),
        ("model.near_limit_rate", Problem::Negative, |d| {
            d.model.near_limit_rate = -0.5;
        }),
        ("model.crash_mce", Problem::Probability, |d| {
            d.model.crash_mce = 1.5;
        }),
        ("model.crash_mce", Problem::Probability, |d| {
            d.model.crash_mce = -0.1;
        }),
        ("model.core_local_bank", Problem::NotFinite, |d| {
            d.model.core_local_bank = f64::NAN;
        }),
        ("model.core_local_bank", Problem::Probability, |d| {
            d.model.core_local_bank = 2.0;
        }),
        ("model.onset_s", Problem::Negative, |d| {
            d.model.onset_s = -1.0;
        }),
        ("model.onset_s", Problem::NotFinite, |d| {
            d.model.onset_s = f64::INFINITY;
        }),
        ("model.onset_boost", Problem::Negative, |d| {
            d.model.onset_boost = -1.0;
        }),
        ("model.onset_boost", Problem::NotFinite, |d| {
            d.model.onset_boost = f64::NAN;
        }),
    ];
    for (path, problem, edit) in cases {
        let mut definition = two_cores();
        edit(&mut definition);
        refused(&definition, path, &problem);
    }
    let mut edge = two_cores();
    edge.model.crash_mce = 1.0;
    edge.model.core_local_bank = 0.0;
    edge.model.growth = 1e-3;
    edge.model.onset_boost = 0.0;
    assert!(Model::new(&edge, 1).is_ok(), "boundary values are valid");
}

#[test]
fn ccd_parameters_are_finite_with_a_nonnegative_slope() {
    let mut definition = two_cores();
    let mut set = |ccd: CcdDef| {
        definition.ccd = Some(ccd);
        invalid(&definition)
    };
    let error = set(CcdDef {
        slope: -1.0,
        ..CcdDef::default()
    });
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("ccd.slope", Problem::Negative)
    );
    let error = set(CcdDef {
        log_rate: f64::INFINITY,
        ..CcdDef::default()
    });
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("ccd.log_rate", Problem::NotFinite)
    );
    let error = set(CcdDef {
        effect: [f64::NAN, 0.0],
        ..CcdDef::default()
    });
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("ccd.effect[0]", Problem::NotFinite)
    );
    let error = set(CcdDef {
        log_rate: 800.0,
        ..CcdDef::default()
    });
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("ccd", Problem::Overflow)
    );
}

#[test]
fn the_first_problem_in_document_order_is_reported() {
    let mut definition = two_cores();
    definition.model.growth = -1.0;
    definition.joint = vec![joint(&[])];
    definition.bios = Some(vec![0]);
    assert_eq!(invalid(&definition).path, "bios");
    definition.bios = None;
    assert_eq!(invalid(&definition).path, "model.growth");
    definition.model.growth = 4.0;
    assert_eq!(invalid(&definition).path, "joint[0].members");
}

#[test]
fn only_signals_a_simulated_failure_can_draw_are_supported() {
    let mut definition = two_cores();
    definition.model.signals = Weights::from([(Signal::UncorrectedMce, 1.0), (Signal::Crash, 1.0)]);
    refused(
        &definition,
        "model.signals.uncorrected_mce",
        &Problem::Unsupported,
    );
    definition.model.signals = Weights::from([(Signal::Crash, 1.0)]);
    definition.model.regime_signals = BTreeMap::from([(
        togi_machine::Regime::R7,
        Weights::from([(Signal::UncorrectedMce, 1.0)]),
    )]);
    refused(
        &definition,
        "model.regime_signals.R7.uncorrected_mce",
        &Problem::Unsupported,
    );
    definition.model.regime_signals.clear();
    let mut bad = joint(&[(0, -5)]);
    bad.signal = Some(Signal::UncorrectedMce);
    definition.joint = vec![bad];
    refused(&definition, "joint[0].signal", &Problem::Unsupported);
    definition.joint.clear();
    definition.script = vec![ScriptDef {
        trial: "0001".to_owned(),
        signal: Some(Signal::UncorrectedMce),
        ..ScriptDef::default()
    }];
    refused(&definition, "script[0].signal", &Problem::Unsupported);
}
