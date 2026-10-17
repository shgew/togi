// Conditions samples: lazy, strictly before the end, stalled culprit, serialized storage and
// shared-voltage lanes. Storage failures are injected without files or devices.

use std::time::Duration;
use togi_machine::{
    CoreId, PerCore, REGIMES, Regime, Signal, TrialConditions, Workload, workloads,
};
use togi_sim::MachineDefinition;
use togi_sim::definition::ScriptDef;

use crate::common::{Rig, committed_machine, core, machine, spec};

fn two_thread_workload() -> (Regime, Workload) {
    REGIMES
        .iter()
        .find_map(|&regime| {
            workloads(regime)
                .iter()
                .find(|workload| workload.threads == 2)
                .map(|workload| (regime, workload.clone()))
        })
        .expect("a two-thread workload")
}

fn cpu(pairs: &[(i64, i64)]) -> PerCore<i64> {
    pairs.iter().map(|&(id, ms)| (core(id), ms)).collect()
}

fn tick(ms: i64, pairs: &[(i64, i64)]) -> TrialConditions {
    TrialConditions {
        elapsed_ms: ms,
        worker_cpu_ms: cpu(pairs),
        ..TrialConditions::default()
    }
}

fn collect(rig: &mut Rig, id: &str) -> Vec<TrialConditions> {
    rig.machine
        .trials
        .samples(id)
        .collect::<Result<_, _>>()
        .expect("valid simulated samples")
}

fn scripted(signal: Signal, at: f64, culprit: i64) -> MachineDefinition {
    let mut definition = machine(8, vec![]);
    definition.script = vec![ScriptDef {
        trial: "0001".to_owned(),
        signal: Some(signal),
        at_s: at,
        core: culprit,
        ..ScriptDef::default()
    }];
    definition
}

fn run_scripted(rig: &mut Rig) {
    let (regime, workload) = two_thread_workload();
    let _ = rig.run(spec(
        "0001",
        regime,
        &workload,
        &[2, 7],
        Duration::from_secs(60),
    ));
}

#[test]
fn samples_tick_each_second_strictly_before_the_end_and_a_stalled_culprit_stops_early() {
    let cases: [(&str, Signal, f64, i64, Vec<TrialConditions>); 6] = [
        (
            "before the first tick",
            Signal::ComputationError,
            0.5,
            2,
            vec![],
        ),
        (
            "at the first tick",
            Signal::ComputationError,
            1.0,
            2,
            vec![],
        ),
        (
            "between ticks",
            Signal::ComputationError,
            2.5,
            2,
            vec![
                tick(1000, &[(2, 2000), (7, 2000)]),
                tick(2000, &[(2, 4000), (7, 4000)]),
            ],
        ),
        (
            "short crash",
            Signal::Crash,
            2.5,
            7,
            vec![
                tick(1000, &[(2, 2000), (7, 2000)]),
                tick(2000, &[(2, 4000), (7, 2000)]),
            ],
        ),
        (
            "fractional crash",
            Signal::Crash,
            4.5,
            7,
            vec![
                tick(1000, &[(2, 2000), (7, 2000)]),
                tick(2000, &[(2, 4000), (7, 4000)]),
                tick(3000, &[(2, 6000), (7, 5000)]),
                tick(4000, &[(2, 8000), (7, 5000)]),
            ],
        ),
        (
            "idle culprit",
            Signal::Crash,
            3.0,
            0,
            vec![
                tick(1000, &[(2, 2000), (7, 2000)]),
                tick(2000, &[(2, 4000), (7, 4000)]),
            ],
        ),
    ];
    for (name, signal, at, culprit, want) in cases {
        for file_backed in [false, true] {
            let mut rig = Rig::new(&scripted(signal, at, culprit), 1);
            let dir = std::rc::Rc::new(crate::common::MemorySamples::default());
            if file_backed {
                rig.sim.set_sample_storage(dir.clone());
            }
            run_scripted(&mut rig);
            rig.sim.reboot();
            assert_eq!(collect(&mut rig, "0001"), want, "{name} file={file_backed}");
            assert_eq!(collect(&mut rig, "0001"), want, "{name}: collected again");
            assert!(
                collect(&mut rig, "missing").is_empty(),
                "{name}: an unknown trial"
            );
        }
    }
}

#[test]
fn a_crashed_trials_serialized_samples_survive_recovery() {
    let mut definition = machine(2, vec![]);
    definition.script = vec![ScriptDef {
        trial: "0001".to_owned(),
        signal: Some(Signal::Crash),
        at_s: 6.0,
        core: 1,
        ..ScriptDef::default()
    }];
    let dir = std::rc::Rc::new(crate::common::MemorySamples::default());
    let mut rig = Rig::new(&definition, 1);
    rig.sim.set_sample_storage(dir.clone());
    let waited = rig
        .run(spec(
            "0001",
            Regime::R7,
            togi_machine::pick_workload(Regime::R7, 0),
            &[0, 1],
            Duration::from_secs(10),
        ))
        .0;
    assert!(crate::common::crashed(&waited));
    rig.sim.reboot();
    let want = vec![
        tick(1000, &[(0, 1000), (1, 1000)]),
        tick(2000, &[(0, 2000), (1, 2000)]),
        tick(3000, &[(0, 3000), (1, 3000)]),
        tick(4000, &[(0, 4000), (1, 4000)]),
        tick(5000, &[(0, 5000), (1, 4000)]),
    ];
    assert_eq!(collect(&mut rig, "0001"), want);
    let mut recovered = Rig::new(&definition, 1);
    recovered.sim.set_sample_storage(dir.clone());
    assert_eq!(collect(&mut recovered, "0001"), want);
}

#[test]
fn ending_a_trial_compresses_its_samples_without_changing_them() {
    let dir = std::rc::Rc::new(crate::common::MemorySamples::default());
    let mut rig = Rig::new(&machine(2, vec![]), 1);
    rig.sim.set_sample_storage(dir.clone());
    rig.pass(spec(
        "0001",
        Regime::R1,
        togi_machine::pick_workload(Regime::R1, 0),
        &[0],
        Duration::from_secs(5),
    ));
    assert!(!dir.files.borrow()["0001"].1, "initially plain JSON lines");
    let before = collect(&mut rig, "0001");
    assert_eq!(before.len(), 4);
    rig.machine.trials.ended("0001").expect("compressed");
    assert!(
        dir.files.borrow()["0001"].1,
        "the stored bytes are compressed"
    );
    assert_eq!(collect(&mut rig, "0001"), before);
    let mut memory = Rig::new(&machine(2, vec![]), 1);
    memory
        .machine
        .trials
        .ended("0001")
        .expect("nothing to compress");
}

#[test]
fn a_long_trial_keeps_no_series_and_yields_lazily() {
    let mut rig = Rig::new(&machine(2, vec![]), 1);
    let hour = Duration::from_secs(3600);
    rig.pass(spec(
        "0001",
        Regime::R1,
        togi_machine::pick_workload(Regime::R1, 0),
        &[0, 1],
        hour,
    ));
    let mut samples = rig.machine.trials.samples("0001");
    assert_eq!(
        samples.next().unwrap().unwrap(),
        tick(1000, &[(0, 1000), (1, 1000)])
    );
    assert_eq!(
        samples.nth(3597).map(|sample| sample.unwrap().elapsed_ms),
        Some(3_599_000)
    );
    assert!(samples.next().is_none(), "strictly before the end");
}

#[test]
fn a_sample_store_that_cannot_be_written_is_an_error_with_the_trial_result() {
    for (case, kind) in [
        ("directory", std::io::ErrorKind::NotADirectory),
        ("file", std::io::ErrorKind::IsADirectory),
    ] {
        let mut rig = Rig::new(&machine(2, vec![]), 1);
        let store = std::rc::Rc::new(crate::common::MemorySamples {
            fail_open: Some(kind),
            ..Default::default()
        });
        rig.sim.set_sample_storage(store);
        let (waited, _) = rig.run(spec(
            "0001",
            Regime::R1,
            togi_machine::pick_workload(Regime::R1, 0),
            &[0],
            Duration::from_secs(3),
        ));
        let Err(partial) = waited else {
            panic!("{case}: persisted");
        };
        assert_eq!(partial.value.ran, Duration::from_secs(3), "{case}");
        assert!(
            matches!(partial.error, togi_machine::Error::Other(_)),
            "{case}"
        );
    }
}

/// A short trial fails when its buffer flushes, a long one while the series streams out.
#[test]
fn a_sample_file_that_refuses_writes_is_an_error_with_the_trial_result() {
    const NO_SPACE: i32 = 28;
    for (case, seconds) in [("short", 3), ("long", 3600)] {
        let mut rig = Rig::new(&machine(2, vec![]), 1);
        rig.sim
            .set_sample_storage(std::rc::Rc::new(crate::common::MemorySamples {
                fail_after: Some(0),
                ..Default::default()
            }));
        let duration = Duration::from_secs(seconds);
        let (waited, _) = rig.run(spec(
            "0001",
            Regime::R1,
            togi_machine::pick_workload(Regime::R1, 0),
            &[0],
            duration,
        ));
        let Err(partial) = waited else {
            panic!("{case}: persisted");
        };
        assert_eq!(partial.value.ran, duration, "{case}");
        let togi_machine::Error::Other(cause) = partial.error else {
            panic!("{case}: not a storage error");
        };
        let io = cause
            .downcast_ref::<std::io::Error>()
            .expect("an I/O error");
        assert_eq!(io.raw_os_error(), Some(NO_SPACE), "{case}");
    }
}

#[test]
fn a_shared_voltage_machine_samples_its_lanes_the_same_lazily_and_in_serialized_storage() {
    let definition = committed_machine("shared-voltage.json");
    let mut memory = Vec::new();
    for file_backed in [false, true] {
        let dir = std::rc::Rc::new(crate::common::MemorySamples::default());
        let mut rig = Rig::new(&definition, 1);
        if file_backed {
            rig.sim.set_sample_storage(dir.clone());
        }
        let workload = togi_machine::pick_workload(Regime::R7, 0);
        rig.pass(spec(
            "0001",
            Regime::R7,
            workload,
            &[0, 1, 7],
            Duration::from_secs(30),
        ));
        rig.sim.reboot();
        let samples = collect(&mut rig, "0001");
        assert_eq!(samples.len(), 29);
        if file_backed {
            assert_eq!(samples, memory);
        } else {
            memory = samples.clone();
        }
        let idle_v = rig.sim.model().voltage().expect("a rail").idle_v;
        for sample in &samples {
            let pm = sample.pm_table.expect("every sample has lanes");
            assert_eq!(sample.core_mhz.len(), 3);
            assert!((f64::from(pm.voltage_request_v[8]) - idle_v).abs() < 1e-6);
            assert!(
                pm.voltage_request_v[7] > pm.voltage_request_v[8],
                "loaded cores request more"
            );
            assert_eq!(
                (pm.c0_pct[0], pm.c0_pct[7], pm.c0_pct[8], pm.cc6_pct[8]),
                (100.0, 100.0, 0.0, 100.0)
            );
        }
        let first = samples[0].core_mhz;
        assert_eq!(first.get(core(0)), first.get(core(1)), "one CCD, one clock");
        assert_eq!(first.get(CoreId::new(8).expect("a core")), None);
    }
}

#[test]
fn the_largest_finite_recorded_idle_voltage_survives_sample_storage() {
    let mut definition = committed_machine("shared-voltage.json");
    definition.shared_voltage.as_mut().expect("a rail").idle_v = f64::from(f32::MAX);
    for stored in [false, true] {
        let mut rig = Rig::new(&definition, 1);
        if stored {
            rig.sim
                .set_sample_storage(std::rc::Rc::new(crate::common::MemorySamples::default()));
        }
        rig.pass(spec(
            "0001",
            Regime::R7,
            togi_machine::pick_workload(Regime::R7, 0),
            &[0, 1],
            Duration::from_secs(3),
        ));
        let persisted = rig.sim.persisted();
        let encoded = serde_json::to_vec(&persisted).expect("serialized machine");
        let decoded: togi_sim::Persisted =
            serde_json::from_slice(&encoded).expect("restorable machine");
        assert_eq!(decoded, persisted);
        let samples = collect(&mut rig, "0001");
        assert_eq!(samples.len(), 2);
        for sample in samples {
            let lanes = sample.pm_table.expect("voltage lanes");
            assert_eq!(lanes.voltage_request_v[8], f32::MAX);
            assert!(
                lanes
                    .voltage_request_v
                    .iter()
                    .all(|value| value.is_finite())
            );
        }
    }
}

#[test]
fn a_machine_without_a_rail_has_no_lanes() {
    let mut rig = Rig::new(&machine(2, vec![]), 1);
    rig.pass(spec(
        "0001",
        Regime::R1,
        togi_machine::pick_workload(Regime::R1, 0),
        &[0],
        Duration::from_secs(3),
    ));
    for sample in collect(&mut rig, "0001") {
        assert!(sample.pm_table.is_none() && sample.core_mhz.is_empty());
    }
}
