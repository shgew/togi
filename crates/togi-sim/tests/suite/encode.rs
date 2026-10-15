// Encoding the materialized machine. Go: TestEncodeMachineRoundTripsEveryField,
// TestEncodeMachineLeavesDefaultModelFieldsOut, TestEncodeMachineRefusesRuntimeOnlyFields (a definition has no
// runtime-only fields, so there is nothing to refuse), TestEncodeMachineRoundTripsControlCharacterStrings,
// TestEncodeMachineRoundTripsLargeWholeFloats, TestEncodeMachineRejectsNonFiniteFloats. The Go tests pinned
// bytes; these pin the properties: what is written loads back equal, and re-encoding is stable.

use proptest::prelude::*;
use std::collections::BTreeMap;
use togi_machine::{Regime, ResetKind, Signal};
use togi_sim::definition::{
    BiosContextDef, CcdDef, CoreDef, JointDef, ModelDef, ScriptDef, Weights,
};
use togi_sim::file::{self, EncodeError};
use togi_sim::{MachineDefinition, Model, Problem, RESETS, WEIGHTED_SIGNALS};

use crate::common::{committed, committed_machine, flat_cores, machine};

fn round_trip(definition: &MachineDefinition) -> String {
    let text = file::encode(definition).expect("encodes");
    assert!(text.ends_with("}\n"), "{text}");
    let back = file::decode(&text).expect("decodes");
    assert_eq!(&back, definition);
    assert_eq!(file::encode(&back).expect("encodes again"), text, "stable");
    text
}

fn full() -> MachineDefinition {
    let mut definition = MachineDefinition {
        description: "every field".to_owned(),
        ..machine(2, flat_cores(2, -30, -29))
    };
    definition.notes = vec!["one".to_owned(), "two".to_owned()];
    definition.facts = Some("../facts/target.jsonl.gz".to_owned());
    definition.bios = Some(vec![0, -1]);
    definition.ranking = Some(vec![9, 7]);
    definition.old_kernel = true;
    definition.bios_context = Some(BiosContextDef {
        bios_version: Some("B.2".to_owned()),
        board: Some("X670".to_owned()),
        cpu_model: Some("Zen 5".to_owned()),
        microcode: Some("0x123".to_owned()),
        boost_limit_mhz: Some(5900),
    });
    definition.model = ModelDef {
        past_limit_rate: 0.7,
        growth: 3.0,
        near_limit_rate: 0.01,
        crash_mce: 0.25,
        core_local_bank: 0.5,
        onset_s: 40.0,
        onset_boost: 2.0,
        signals: Weights::from([(Signal::Crash, 1.0), (Signal::Stall, 2.0)]),
        regime_signals: BTreeMap::from([(
            Regime::R7,
            Weights::from([(Signal::Crash, 79.0), (Signal::ComputationError, 5.0)]),
        )]),
        reset: Weights::from([(ResetKind::ThermalTrip, 1.0), (ResetKind::PowerLoss, 3.0)]),
    };
    definition.ccd = Some(CcdDef {
        log_rate: -7.0,
        slope: 0.05,
        effect: [0.5, -0.5],
    });
    definition.core[0].flat = 0.1;
    definition.core[0].idle = Some(-45);
    definition.core[0].workload = BTreeMap::from([("special".to_owned(), -27)]);
    definition.joint = vec![JointDef {
        members: [(0, -20), (1, -20)].into(),
        regimes: vec![Regime::R6, Regime::R7],
        rate: Some(0.05),
        after_s: 30.0,
        signal: Some(Signal::Crash),
        crash_mce_core: Some(1),
    }];
    definition.script = vec![ScriptDef {
        trial: "0042".to_owned(),
        signal: Some(Signal::Crash),
        at_s: 3.0,
        core: 1,
        reset: Some(ResetKind::Watchdog),
        then_crash: true,
    }];
    definition
}

#[test]
fn every_field_round_trips() {
    round_trip(&full());
}

#[test]
fn the_default_machine_is_an_empty_object() {
    assert_eq!(round_trip(&MachineDefinition::default()), "{}\n");
}

#[test]
fn fields_equal_to_their_default_are_left_out_and_the_rest_are_written() {
    let mut definition = machine(16, vec![]);
    definition.model.growth = 4.0;
    definition.model.onset_boost = 0.0;
    definition.model.crash_mce = 0.6;
    definition.model.signals = Weights::from([
        (Signal::ComputationError, 4.0),
        (Signal::Stall, 1.0),
        (Signal::UnexpectedExit, 1.0),
        (Signal::CorrectedMce, 2.0),
        (Signal::Crash, 2.0),
    ]);
    let text = round_trip(&definition);
    for left_out in [
        "cores",
        "growth",
        "onset_boost",
        "signals",
        "reset",
        "past_limit_rate",
    ] {
        assert!(!text.contains(left_out), "{left_out} in {text}");
    }
    assert!(text.contains("\"crash_mce\": 0.6"), "{text}");
    let eight = round_trip(&machine(8, vec![]));
    assert!(eight.contains("\"cores\": 8"), "{eight}");
    let mut ccd = machine(16, vec![]);
    ccd.ccd = Some(CcdDef {
        log_rate: -7.0,
        slope: 0.0,
        effect: [0.0, 0.5],
    });
    let text = round_trip(&ccd);
    assert!(text.contains("\"log_rate\": -7.0"), "{text}");
    assert!(text.contains("\"effect\""), "{text}");
    assert!(!text.contains("slope"), "{text}");
    ccd.ccd = Some(CcdDef::default());
    let text = round_trip(&ccd);
    assert!(text.contains("\"ccd\": {}"), "{text}");
    for left_out in ["log_rate", "slope", "effect"] {
        assert!(!text.contains(left_out), "{left_out} in {text}");
    }
}

#[test]
fn the_text_is_pretty_with_sorted_maps() {
    let text = round_trip(&full());
    assert!(
        text.starts_with("{\n  \"description\": \"every field\""),
        "{text}"
    );
    assert!(text.contains("\n      \"R7\": {\n        \"computation_error\": 5.0,\n        \"crash\": 79.0\n      }"), "{text}");
    assert!(
        text.find("\"0\": -20").expect("member 0") < text.find("\"1\": -20").expect("member 1")
    );
}

#[test]
fn awkward_strings_round_trip() {
    let mut definition = MachineDefinition {
        description: "tab\t quote\" backslash\\ newline\n bell\u{7} nul\u{0} é 日本 😀 \u{2028}"
            .to_owned(),
        ..machine(2, vec![])
    };
    definition.notes = vec![String::new(), "</script> & <b>".to_owned()];
    definition.facts = Some("dir with spaces/\u{e9}.jsonl.gz".to_owned());
    definition.script = vec![ScriptDef {
        trial: "t\"\\\u{1}".to_owned(),
        ..ScriptDef::default()
    }];
    definition.core = flat_cores(2, -10, -10);
    definition.core[0].workload = BTreeMap::from([("w\"\n\u{1f}".to_owned(), -9)]);
    round_trip(&definition);
}

#[test]
fn large_and_small_finite_floats_keep_their_value() {
    let mut definition = machine(2, flat_cores(2, -10, -10));
    definition.core[0].flat = 1e21;
    definition.core[1].flat = 1.844_674_407_370_955_2e19;
    definition.model.past_limit_rate = 5e-324;
    definition.model.near_limit_rate = f64::MAX / 1e300;
    definition.model.growth = 1.0;
    definition.model.onset_s = 123_456_789_012_345_680_000.0;
    definition.ccd = Some(CcdDef {
        log_rate: -123_456_789_012_345_680_000.0,
        slope: 0.0,
        effect: [-1e21, 0.1 + 0.2],
    });
    definition.bios = None;
    round_trip(&definition);
}

#[test]
fn an_invalid_machine_is_refused_before_anything_is_written() {
    let mut definition = full();
    definition.model.growth = f64::NAN;
    match file::encode(&definition) {
        Err(EncodeError::Invalid(error)) => {
            assert_eq!(
                (error.path.as_str(), error.problem),
                ("model.growth", Problem::NotFinite)
            );
        }
        other => panic!("{other:?}"),
    }
    for (edit, path) in [
        (
            (|d: &mut MachineDefinition| d.core[0].flat = f64::INFINITY)
                as fn(&mut MachineDefinition),
            "core[0].flat",
        ),
        (
            |d| d.joint[0].after_s = f64::NEG_INFINITY,
            "joint[0].after_s",
        ),
        (
            |d| d.ccd.as_mut().expect("ccd").effect[1] = f64::NAN,
            "ccd.effect[1]",
        ),
        (|d| d.model.signals = Weights::default(), "model.signals"),
        (|d| d.bios = Some(vec![0]), "bios"),
    ] {
        let mut definition = full();
        edit(&mut definition);
        let Err(EncodeError::Invalid(error)) = file::encode(&definition) else {
            panic!("{path} was encoded");
        };
        assert_eq!(error.path, path);
    }
    let mut voltage = committed_machine("shared-voltage.json");
    voltage.shared_voltage.as_mut().expect("a rail").margin_v = f64::NAN;
    assert!(matches!(
        file::encode(&voltage),
        Err(EncodeError::Invalid(_))
    ));
}

#[test]
fn every_committed_machine_round_trips_through_the_encoder() {
    for path in committed() {
        let loaded = file::load_from(&path, &crate::common::committed_files())
            .map(|(machine, _)| machine)
            .expect("loads");
        let text = file::encode(&loaded).expect("encodes");
        assert_eq!(
            file::decode(&text).expect("decodes"),
            loaded,
            "{}",
            path.display()
        );
        let again = file::decode(&text).expect("decodes");
        assert!(Model::new(&again, 1).is_ok(), "{}", path.display());
    }
}

fn weights<K: Ord + Copy + std::fmt::Debug + 'static>(
    kinds: &'static [K],
) -> impl Strategy<Value = Weights<K>> {
    (
        proptest::sample::select(kinds),
        0.5..50.0f64,
        proptest::collection::btree_map(proptest::sample::select(kinds), 0.0..50.0f64, 0..4),
    )
        .prop_map(|(positive, weight, mut rest)| {
            rest.insert(positive, weight);
            Weights(rest)
        })
}

fn core_defs(cores: i64) -> impl Strategy<Value = Vec<CoreDef>> {
    proptest::collection::vec(
        (
            proptest::array::uniform5(-50..=1i64),
            proptest::array::uniform7(-50..=1i64),
            0.0..1.0f64,
            proptest::option::of(-50..=1i64),
            proptest::collection::btree_map("[a-z0-9-]{1,8}", -50..=1i64, 0..3),
        ),
        usize::try_from(cores).expect("cores")..=usize::try_from(cores).expect("cores"),
    )
    .prop_map(|limits| {
        limits
            .into_iter()
            .enumerate()
            .map(|(id, (alone, together, flat, idle, workload))| CoreDef {
                id: i64::try_from(id).expect("id"),
                alone,
                together,
                flat,
                idle,
                workload,
            })
            .collect()
    })
}

fn joints(cores: i64) -> impl Strategy<Value = Vec<JointDef>> {
    proptest::collection::vec(
        (
            proptest::collection::btree_map(0..cores, -50..=0i64, 1..4),
            proptest::collection::vec(proptest::sample::select(&togi_machine::REGIMES[..]), 0..3),
            proptest::option::of(0.0..1.0f64),
            0.0..300.0f64,
            proptest::option::of(proptest::sample::select(&WEIGHTED_SIGNALS[..])),
            proptest::option::of(0..cores),
        ),
        0..3,
    )
    .prop_map(|joints| {
        joints
            .into_iter()
            .map(
                |(members, regimes, rate, after_s, signal, crash_mce_core)| JointDef {
                    members,
                    regimes,
                    rate,
                    after_s,
                    signal,
                    crash_mce_core,
                },
            )
            .collect()
    })
}

fn models() -> impl Strategy<Value = ModelDef> {
    (
        (
            0.0..10.0f64,
            0.1..5.0f64,
            0.0..1.0f64,
            0.0..=1.0f64,
            0.0..=1.0f64,
        ),
        (0.0..500.0f64, 0.0..10.0f64),
        weights(&WEIGHTED_SIGNALS),
        proptest::collection::btree_map(
            proptest::sample::select(&togi_machine::REGIMES[..]),
            weights(&WEIGHTED_SIGNALS),
            0..3,
        ),
        weights(&RESETS),
    )
        .prop_map(
            |(
                (past_limit_rate, growth, near_limit_rate, crash_mce, core_local_bank),
                (onset_s, onset_boost),
                signals,
                regime_signals,
                reset,
            )| ModelDef {
                past_limit_rate,
                growth,
                near_limit_rate,
                crash_mce,
                core_local_bank,
                onset_s,
                onset_boost,
                signals,
                regime_signals,
                reset,
            },
        )
}

fn definitions() -> impl Strategy<Value = MachineDefinition> {
    (1..=8i64)
        .prop_flat_map(|half| {
            let cores = half * 2;
            (
                Just(cores),
                (
                    "\\PC{0,12}",
                    proptest::collection::vec("\\PC{0,12}", 0..3),
                    proptest::option::of("\\PC{0,12}"),
                    proptest::option::of(proptest::collection::vec(
                        -50..=0i64,
                        usize::try_from(cores).expect("cores"),
                    )),
                    proptest::option::of(proptest::collection::vec(
                        -5..50i64,
                        usize::try_from(cores).expect("cores"),
                    )),
                    any::<bool>(),
                ),
                models(),
                proptest::option::of(
                    (-10.0..0.0f64, 0.0..0.5f64, [-1.0..1.0f64, -1.0..1.0f64]).prop_map(
                        |(log_rate, slope, effect)| CcdDef {
                            log_rate,
                            slope,
                            effect,
                        },
                    ),
                ),
                prop_oneof![Just(vec![]), core_defs(cores)],
                joints(cores),
                (0..4usize).prop_map(move |count| {
                    (0..count)
                        .map(|index| ScriptDef {
                            trial: format!("{index:04}"),
                            signal: Some(Signal::Stall),
                            at_s: 1.5,
                            core: i64::try_from(index).expect("index") % cores,
                            reset: Some(ResetKind::SyncFlood),
                            then_crash: index % 2 == 0,
                        })
                        .collect::<Vec<_>>()
                }),
            )
        })
        .prop_map(
            |(
                cores,
                (description, notes, facts, bios, ranking, old_kernel),
                model,
                ccd,
                core,
                joint,
                script,
            )| {
                MachineDefinition {
                    description,
                    notes,
                    cores,
                    facts,
                    bios,
                    ranking,
                    old_kernel,
                    bios_context: None,
                    model,
                    ccd,
                    core,
                    joint,
                    script,
                    shared_voltage: None,
                }
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(953),
        ..ProptestConfig::default()
    })]

    #[test]
    fn every_valid_definition_round_trips(definition in definitions()) {
        prop_assert!(Model::new(&definition, 1).is_ok());
        let text = file::encode(&definition).expect("encodes");
        let back = file::decode(&text).expect("decodes");
        prop_assert_eq!(&back, &definition);
        prop_assert_eq!(file::encode(&back).expect("encodes again"), text);
    }
}
