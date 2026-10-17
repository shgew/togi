// The machine-file document: what decodes, what is refused as malformed, and the defaults.

use std::collections::BTreeMap;
use std::time::Duration;
use togi_machine::{BiosContext, Regime, ResetKind, Signal};
use togi_sim::definition::{DEFAULT_GROWTH, DEFAULT_ONSET_S, ScriptDef, Weights};
use togi_sim::file::{self, LoadError};
use togi_sim::model::Script;
use togi_sim::{MachineDefinition, Model, RunSettings, WEIGHTED_SIGNALS};

use crate::common::{Rig, basic, core, machine, model};

const FULL: &str = r#"{
  "description": "Fixture machine",
  "notes": ["Generated fixture provenance"],
  "cores": 2,
  "bios": [0, -1],
  "ranking": [9, 7],
  "old_kernel": true,
  "model": {
    "past_limit_rate": 0.7,
    "onset_boost": 2,
    "signals": {"crash": 1},
    "regime_signals": {"R7": {"crash": 79, "computation_error": 5}},
    "reset": {"thermal_trip": 1}
  },
  "core": [
    {
      "id": 0,
      "alone": [-30, -30, -30, -30, -30],
      "together": [-29, -29, -29, -29, -29, -29, -29],
      "idle": -45,
      "workload": {"mprime-sse-4k-21k": -27},
      "flat": 0.1
    },
    {
      "id": 1,
      "alone": [-30, -30, -30, -30, -30],
      "together": [-29, -29, -29, -29, -29, -29, -29]
    }
  ],
  "joint": [
    {"members": {"0": -20, "1": -20}, "regimes": ["R7"], "rate": 0.05, "after_s": 30, "signal": "crash"}
  ],
  "script": [
    {"trial": "0042", "signal": "crash", "at_s": 3, "core": 1, "reset": "watchdog", "then_crash": true}
  ]
}"#;

#[test]
fn a_full_machine_decodes_with_its_metadata() {
    let definition = file::decode(FULL).expect("decodes");
    assert_eq!(definition.description, "Fixture machine");
    assert_eq!(definition.notes, ["Generated fixture provenance"]);
    assert_eq!(definition.ranking, Some(vec![9, 7]));
    assert_eq!(definition.model.past_limit_rate, 0.7);
    assert_eq!(definition.model.onset_s, DEFAULT_ONSET_S);
    assert_eq!(definition.model.growth, DEFAULT_GROWTH);
    assert_eq!(
        definition.model.reset,
        Weights::from([(ResetKind::ThermalTrip, 1.0)])
    );
    assert_eq!(
        definition.model.regime_signals,
        BTreeMap::from([(
            togi_machine::Regime::R7,
            Weights::from([(Signal::Crash, 79.0), (Signal::ComputationError, 5.0)])
        )])
    );
    assert_eq!(
        definition.core[0].workload[&"mprime-sse-4k-21k".parse().unwrap()],
        -27
    );
    assert_eq!(definition.joint[0].after_s, 30.0);
    assert!(definition.script[0].then_crash);

    let model = model(&definition);
    assert_eq!(model.cores(), 2);
    assert_eq!(model.ranking(), Some(&[9, 7][..]));
    assert!(model.old_kernel());
    assert!(model.has_regime_signals());
    assert_eq!(model.signal_mix(Regime::R7).weight(Signal::Crash), 79.0);
    assert_eq!(model.signal_mix(Regime::R1).weight(Signal::Crash), 1.0);
    assert_eq!(model.reset_mix().weight(ResetKind::ThermalTrip), 1.0);
    assert_eq!(
        model.script("0042").map(|script| script.then_crash),
        Some(true)
    );
}

fn load_text(text: &str) -> Result<MachineDefinition, LoadError> {
    let dir = crate::common::MemoryFiles::default();
    let path = dir.write(&dir.path().join("machine.json"), text);
    file::load_from(&path, &dir).map(|(machine, _)| machine)
}

#[test]
fn unknown_workload_limit_ids_are_refused() {
    let text = FULL.replace("mprime-sse-4k-21k", "mprime-sse-4k-21k-typo");
    let error =
        file::decode(&text).expect_err("unknown workload limit must not silently fall back");
    assert!(error.to_string().contains("unknown workload"), "{error}");
}

#[test]
fn malformed_documents_are_refused_with_the_file_named() {
    let core = r#"{"id":0,"alone":[-10,-10,-10,-10,-10],"together":[-10,-10,-10,-10,-10,-10,-10]}"#;
    let cases = [
        ("syntax", r#"{"cores":["#.to_owned(), "EOF"),
        ("null root", "null".to_owned(), "expected"),
        (
            "null root with whitespace",
            " \n\tnull\r\n".to_owned(),
            "expected",
        ),
        ("array root", "[]".to_owned(), "expected"),
        (
            "unknown field",
            r#"{"cores":2,"unknown":1}"#.to_owned(),
            "unknown field",
        ),
        (
            "unknown nested field",
            r#"{"cores":2,"model":{"unknown":1}}"#.to_owned(),
            "unknown field",
        ),
        ("trailing value", r#"{"cores":2} {}"#.to_owned(), "trailing"),
        (
            "duplicate key",
            r#"{"cores":2,"cores":4}"#.to_owned(),
            "duplicate",
        ),
        ("wrong case", r#"{"Cores":2}"#.to_owned(), "unknown field"),
        (
            "short fixed array",
            r#"{"cores":2,"ccd":{"effect":[0]}}"#.to_owned(),
            "length",
        ),
        (
            "long fixed array",
            r#"{"cores":2,"ccd":{"effect":[0,0,0]}}"#.to_owned(),
            "trailing characters",
        ),
        (
            "short limit array",
            r#"{"cores":2,"core":[{"id":0,"alone":[-10]}]}"#.to_owned(),
            "length",
        ),
        (
            "duplicate joint member",
            r#"{"cores":2,"joint":[{"members":{"0":-10,"0":-20}}]}"#.to_owned(),
            "duplicate",
        ),
        (
            "duplicate workload limit",
            format!(
                r#"{{"cores":2,"core":[{}]}}"#,
                core.replace(
                    '}',
                    r#","workload":{"mprime-sse-4k-21k":-1,"mprime-sse-4k-21k":-2}}"#
                )
            ),
            "duplicate",
        ),
        (
            "duplicate signal",
            r#"{"model":{"signals":{"crash":1,"crash":2}}}"#.to_owned(),
            "duplicate",
        ),
        (
            "duplicate regime",
            r#"{"model":{"regime_signals":{"R7":{"crash":1},"R7":{"crash":1}}}}"#.to_owned(),
            "duplicate",
        ),
        (
            "duplicate signal within a regime",
            r#"{"model":{"regime_signals":{"R7":{"crash":1,"crash":1}}}}"#.to_owned(),
            "duplicate",
        ),
        (
            "joint member that is no number",
            r#"{"cores":2,"joint":[{"members":{"nope":-10}}]}"#.to_owned(),
            "expected key to be a number",
        ),
        (
            "unsupported regime",
            r#"{"joint":[{"members":{"0":-5},"regimes":["R9"]}]}"#.to_owned(),
            "unknown variant `R9`",
        ),
        (
            "unsupported joint signal",
            r#"{"joint":[{"members":{"0":-5},"signal":"crsh"}]}"#.to_owned(),
            "unknown variant `crsh`",
        ),
        (
            "unsupported regime in regime_signals",
            r#"{"model":{"regime_signals":{"R9":{"crash":1}}}}"#.to_owned(),
            "unknown variant `R9`",
        ),
        (
            "unsupported script signal",
            r#"{"script":[{"trial":"0001","signal":"crsh"}]}"#.to_owned(),
            "unknown variant `crsh`",
        ),
        (
            "unsupported script reset",
            r#"{"script":[{"trial":"0001","reset":"brownout"}]}"#.to_owned(),
            "unknown variant `brownout`",
        ),
        (
            "unsupported reset",
            r#"{"model":{"reset":{"brownout":1}}}"#.to_owned(),
            "unknown variant `brownout`",
        ),
    ];
    for (name, text, want) in cases {
        let error = load_text(&text).expect_err(name);
        let message = error.to_string();
        assert!(
            matches!(error, LoadError::Parse { .. }),
            "{name}: {message}"
        );
        assert!(message.contains("machine.json"), "{name}: {message}");
        assert!(message.contains(want), "{name}: {message} lacks {want:?}");
    }
}

#[test]
fn an_empty_object_is_the_default_machine() {
    let definition = file::decode(" {} \n").expect("decodes");
    assert_eq!(definition, MachineDefinition::default());
    let model = model(&definition);
    assert_eq!(model.cores(), 16);
    assert_eq!(model.limits().len(), 16, "limits are drawn from the seed");
    assert!(model.bios().iter().all(|offset| offset.get() == 0));
    assert_eq!(model.ranking(), None);
    assert!(!model.has_regime_signals());
    assert!(model.ccd().is_none() && model.voltage().is_none() && model.joints().is_empty());
    let params = model.params();
    assert_eq!(params.growth, 4.0);
    assert_eq!(params.crash_mce, 0.5);
    assert_eq!(params.core_local_bank, 0.8);
    assert_eq!(params.onset.window_s, 100.0);
    assert_eq!(params.onset.boost, 0.0);
    assert_eq!(params.near_limit_rate, 0.0);
    assert!((params.past_limit_rate - 10f64.ln() / 90.0).abs() < 1e-15);
    assert_eq!(model.reset_mix().weight(ResetKind::Watchdog), 1.0);
    let signals = model.signal_mix(Regime::R1);
    let weights = WEIGHTED_SIGNALS.map(|signal| signals.weight(signal));
    assert_eq!(weights, [4.0, 1.0, 1.0, 2.0, 2.0]);
}

#[test]
fn the_default_bios_context_fills_what_a_file_leaves_out() {
    let simulator = BiosContext {
        bios_version: "SIM.1".to_owned(),
        board: "togi simulator".to_owned(),
        cpu_model: "Simulated Zen 5 16-Core Processor".to_owned(),
        microcode: "0x0".to_owned(),
        boost_limit_mhz: 5700,
    };
    let context = |text: &str| {
        let definition = file::decode(text).expect("decodes");
        model(&definition).bios_context().clone()
    };
    assert_eq!(context(r#"{"cores":2}"#), simulator);
    let present = r#"{"cores":2,"bios_context":{"bios_version":"B.2","board":"X670","cpu_model":"Zen 5","microcode":"0x123","boost_limit_mhz":5900}}"#;
    assert_eq!(
        context(present),
        BiosContext {
            bios_version: "B.2".to_owned(),
            board: "X670".to_owned(),
            cpu_model: "Zen 5".to_owned(),
            microcode: "0x123".to_owned(),
            boost_limit_mhz: 5900,
        }
    );
    assert_eq!(
        context(r#"{"cores":2,"bios_context":{"bios_version":"SIM.2"}}"#),
        BiosContext {
            bios_version: "SIM.2".to_owned(),
            ..simulator
        }
    );
}

#[test]
fn a_model_keeps_what_it_was_built_from_by_value() {
    let mut definition = machine(2, crate::common::flat_cores(2, -20, -20));
    definition.script = vec![ScriptDef {
        trial: "0001".to_owned(),
        signal: Some(Signal::Stall),
        at_s: 1.0,
        core: 1,
        ..ScriptDef::default()
    }];
    let model = Model::new(&definition, 1).expect("valid");
    definition.core[0].alone = [-5; 5];
    definition.script.clear();
    assert_eq!(model.limits()[0].alone, [-20; 5]);
    assert_eq!(
        model.script("0001"),
        Some(&Script {
            signal: Some(Signal::Stall),
            at_s: 1.0,
            core: core(1),
            reset: None,
            then_crash: false,
        })
    );
    let mut rig = Rig::with(model, RunSettings::default());
    let result = rig.pass(basic(Regime::R1, &[0, 1], Duration::from_secs(60)));
    let failure = result.failure.expect("the scripted failure");
    assert_eq!((failure.signal, failure.core), (Signal::Stall, core(1)));
    assert_eq!(result.ran, Duration::from_secs(1));
}
