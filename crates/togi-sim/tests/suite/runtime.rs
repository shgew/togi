// Trial outcomes, determinism, faults and reboots through the seams.

use jiff::SignedDuration;
use std::time::Duration;
use togi_machine::{
    BankType, BiosContext, CancellationToken, Check, CpuId, Error, ObservedOffset, Partial, Regime,
    Signal, TargetOffset, pick_workload,
};
use togi_sim::definition::{ModelDef, Weights};
use togi_sim::{History, RunSettings, Simulator, WEIGHTED_SIGNALS};

use test_case::test_case;

use crate::common::{Rig, basic, core, crashed, flat_cores, machine, model, sharp, spec};

const NINETY: Duration = Duration::from_secs(90);

fn two_cores(model: ModelDef) -> togi_sim::MachineDefinition {
    let mut definition = machine(2, flat_cores(2, -10, -10));
    definition.model = model;
    definition
}

#[test]
fn r6_progress_counts_the_bursts_of_the_whole_trial() {
    let mut rig = Rig::new(&machine(16, flat_cores(16, -10, -10)), 3);
    let all: Vec<i64> = (0..16).collect();
    let (waited, recorder) = rig.run(basic(Regime::R6, &all, NINETY));
    let result = waited.expect("a pass");
    assert_eq!(
        recorder.progress,
        [
            "first half idle, then 100ms bursts every 2s, one core at a time",
            "bursts: 23 continues, 39 stops"
        ]
    );
    assert_eq!((result.stops, result.conts), (39, 23));
}

#[test]
fn an_r6_failure_before_the_transition_reports_only_its_bursts() {
    let mut definition = machine(16, flat_cores(16, 0, 0));
    definition.model = sharp(Signal::UnexpectedExit);
    let mut rig = Rig::new(&definition, 3);
    rig.set(0, -1).expect("set");
    let (waited, recorder) = rig.run(basic(Regime::R6, &[0], NINETY));
    let result = waited.expect("a result");
    assert!(result.ran < NINETY / 2, "failed at {:?}", result.ran);
    assert_eq!(recorder.progress, ["bursts: 0 continues, 1 stops"]);
}

fn transcript(seed: u64) -> Vec<String> {
    let mut rig = Rig::new(&machine(4, vec![]), seed);
    let mut out = Vec::new();
    let boot = rig.boot();
    out.push(format!("boot {boot} limits {:?}", rig.sim.model().limits()));
    for index in 0..4 {
        let limit = rig.sim.model().alone_limit(core(index));
        for value in [limit - 2, limit, limit + 1] {
            for regime in [Regime::R1, Regime::R3, Regime::R4] {
                let waited = rig.alone(index, TargetOffset::clamped(value).get(), regime, 0);
                out.push(format!(
                    "core {index} offset {value} {regime}: {waited:?} at {}",
                    rig.sim.now()
                ));
                if crashed(&waited) {
                    rig.sim.reboot().expect("a representable reboot");
                }
            }
        }
    }
    let first = rig.boot();
    rig.sim.crash();
    rig.sim.reboot().expect("a representable reboot");
    let second = rig.boot();
    for boot in [boot, first, second] {
        out.push(format!("boot {boot} mces {:?}", rig.mces(&boot)));
    }
    out
}

#[test]
fn the_same_seed_makes_the_same_observations_and_another_seed_does_not() {
    let a = transcript(7);
    assert_eq!(a, transcript(7));
    assert!(a.len() > 36);
    assert_ne!(a[0], transcript(8)[0], "limits and boot ID follow the seed");
}

#[test]
fn the_same_seed_and_history_make_the_same_observations() {
    let history = || History {
        boots: 5,
        last_time: Some(jiff::Timestamp::from_second(1_800_000_000).expect("a time")),
        bios_context: None,
    };
    let run = |seed| {
        let settings = RunSettings {
            seed,
            history: history(),
            replay: None,
        };
        let mut rig = Rig::with(model(&machine(4, vec![])), settings);
        let boot = rig.boot();
        let waited = rig.alone(0, -45, Regime::R1, 0);
        (boot, format!("{waited:?}"), rig.sim.now())
    };
    assert_eq!(run(2), run(2));
    assert_ne!(run(2).0, run(3).0);
}

#[test]
fn each_signal_has_its_result_shape() {
    for signal in WEIGHTED_SIGNALS {
        let mut definition = two_cores(ModelDef {
            crash_mce: 1.0,
            ..sharp(signal)
        });
        definition.bios = Some(vec![-3, -3]);
        let mut rig = Rig::new(&definition, 1);
        let boot = rig.boot();
        let waited = rig.alone(0, -12, Regime::R2, 0);
        match signal {
            Signal::ComputationError | Signal::Stall | Signal::UnexpectedExit => {
                let result = waited.unwrap_or_else(|error| panic!("{signal}: {error:?}"));
                let Some(failure) = result.failure else {
                    panic!("{signal}: no failure in {result:?}");
                };
                assert_eq!(
                    (failure.signal, failure.core),
                    (signal, core(0)),
                    "{signal}"
                );
                assert!(result.ran < NINETY, "{signal}: {:?}", result.ran);
            }
            Signal::CorrectedMce => {
                let result = waited.unwrap_or_else(|error| panic!("{signal}: {error:?}"));
                let mces = rig.mces(&boot);
                assert!(result.failure.is_none(), "{signal}: {:?}", result.failure);
                assert_eq!(result.ran, NINETY, "{signal}");
                assert_eq!(mces.len(), 1, "{signal}: {mces:?}");
                assert_eq!(
                    (mces[0].corrected, mces[0].core),
                    (true, Some(core(0))),
                    "{signal}"
                );
            }
            Signal::Crash => {
                assert!(crashed(&waited), "{signal}: {waited:?}");
                assert!(matches!(rig.machine.host.boot_id(), Err(Error::Crashed)));
                rig.sim.reboot().expect("a representable reboot");
                let next = rig.boot();
                let mces = rig.mces(&next);
                assert_eq!(mces.len(), 1, "{signal}: {mces:?}");
                assert_eq!(
                    (mces[0].corrected, mces[0].core),
                    (false, Some(core(0))),
                    "{signal}"
                );
                assert_eq!(mces[0].time, rig.sim.now(), "{signal}");
                assert_eq!(mces[0].monotonic, Duration::ZERO, "{signal}");
            }
            Signal::UncorrectedMce => unreachable!("not a weighted signal"),
        }
    }
}

#[test_case(0.0; "unset")]
#[test_case(1e6; "set")]
fn the_near_limit_rate_fails_a_core_at_its_limit_only_when_it_is_set(rate: f64) {
    let definition = two_cores(ModelDef {
        near_limit_rate: rate,
        ..sharp(Signal::ComputationError)
    });
    let mut rig = Rig::new(&definition, 2);
    for index in 0..100 {
        let result = rig.alone(1, -10, Regime::R1, index).expect("a result");
        assert_eq!(result.failure.is_some(), rate > 0.0, "trial {index}");
    }
}

#[test]
fn a_nonzero_outsider_switches_a_trial_to_the_together_limit() {
    let mut definition = machine(2, flat_cores(2, -20, -15));
    definition.model = sharp(Signal::ComputationError);
    let mut rig = Rig::new(&definition, 3);
    assert!(
        rig.alone(0, -18, Regime::R1, 0)
            .expect("a result")
            .failure
            .is_none()
    );
    rig.set(1, -1).expect("set");
    let result = rig.pass(basic(Regime::R1, &[0], NINETY));
    assert_eq!(
        result.failure.map(|failure| failure.signal),
        Some(Signal::ComputationError)
    );
}

#[test_case(1.0; "core local")]
#[test_case(0.0; "shared")]
fn machine_checks_name_a_bank_of_the_kind_the_model_chooses(local: f64) {
    const BANKS: [BankType; 9] = [
        BankType::LoadStore,
        BankType::InstructionFetch,
        BankType::L2Cache,
        BankType::DecodeUnit,
        BankType::ExecutionUnit,
        BankType::FloatingPoint,
        BankType::L3Cache,
        BankType::MemoryController,
        BankType::DataFabric,
    ];
    let definition = two_cores(ModelDef {
        core_local_bank: local,
        ..sharp(Signal::CorrectedMce)
    });
    let mut rig = Rig::new(&definition, 4);
    for index in 0..20 {
        rig.alone(0, -15, Regime::R2, index).expect("a result");
    }
    let boot = rig.boot();
    let mces = rig.mces(&boot);
    assert_eq!(mces.len(), 20);
    for mce in mces {
        assert_eq!(mce.bank_type.core_local(), local > 0.5);
        assert_eq!(BANKS[usize::from(mce.bank)], mce.bank_type);
    }
}

#[test]
fn a_readback_can_be_corrupted_once_after_the_next_write() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 5);
    rig.sim.corrupt_readback(core(1));
    assert_eq!(
        rig.read(1).expect("read"),
        ObservedOffset(0),
        "before a write"
    );
    rig.set_all(-7).expect("write");
    assert_eq!(rig.read(1).expect("read"), ObservedOffset(-8));
    assert_eq!(rig.read(1).expect("read"), ObservedOffset(-7), "only once");
    rig.sim.corrupt_readback(core(0));
    rig.set(0, -50).expect("write");
    assert_eq!(
        rig.read(0).expect("read"),
        ObservedOffset(-49),
        "stays in range at the floor"
    );
}

#[test]
fn a_failed_write_leaves_the_register_alone_and_costs_one_write() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 5);
    rig.sim.fail_write(1);
    assert!(rig.set(0, -5).is_err());
    assert_eq!(rig.read(0).expect("read"), ObservedOffset(0));
    rig.set(0, -5).expect("the second write");
    assert!(
        TargetOffset::new(1).is_err(),
        "a positive offset cannot be written"
    );
}

#[test]
fn setup_failures_fail_the_next_starts_only() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 5);
    rig.sim.fail_setup(2);
    for attempt in 0..3 {
        let started = rig.start(basic(Regime::R1, &[0], NINETY));
        assert_eq!(started.is_err(), attempt < 2, "attempt {attempt}");
    }
}

#[test]
fn a_worker_escapes_its_cpus_in_one_trial() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 5);
    rig.sim.escape();
    assert_eq!(
        rig.pass(basic(Regime::R1, &[1], NINETY)).escaped,
        [CpuId::from(5)]
    );
    assert!(rig.pass(basic(Regime::R1, &[1], NINETY)).escaped.is_empty());
}

#[test]
fn a_failed_check_shows_in_the_preflight_until_cleared() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 5);
    rig.sim.fail_check("root", "uid 1000");
    let checks = rig.machine.host.preflight();
    assert_eq!(
        checks[0],
        Check {
            name: "root".to_owned(),
            detail: "uid 1000".to_owned(),
            ok: false
        }
    );
    assert!(checks[1].ok);
    rig.sim.fail_check("root", "");
    assert!(rig.machine.host.preflight()[0].ok);
}

#[test]
fn the_bios_context_can_change_under_a_running_machine() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 5);
    let changed = BiosContext {
        bios_version: "SIM.2".to_owned(),
        ..rig.machine.host.bios_context().expect("a context")
    };
    rig.sim.set_bios_context(changed.clone());
    assert_eq!(rig.machine.host.bios_context().expect("a context"), changed);
}

#[test]
fn a_crash_before_apply_takes_the_first_write_of_the_boot() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 5);
    rig.sim.crash_before_apply(1);
    rig.read(0).expect("a read before the first write");
    assert!(matches!(rig.set_all(0), Err(Error::Crashed)));
    rig.sim.reboot().expect("a representable reboot");
    rig.set_all(0).expect("the first write of the next boot");
}

#[test]
fn a_trial_alone_with_an_outsider_at_a_nonzero_offset_is_a_violation() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 5);
    rig.set(1, -4).expect("set");
    let mut alone = spec(
        "0007",
        Regime::R1,
        pick_workload(Regime::R1, 0),
        &[0],
        Duration::from_secs(1),
    );
    alone.condition = togi_machine::Condition::Alone;
    let running = rig.start(alone).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(running.started().pid, 1007);
    assert_eq!(rig.sim.violations().len(), 1);
}

#[test]
fn a_reboot_restores_the_bios_offsets_and_takes_ninety_seconds() {
    let mut definition = machine(2, vec![]);
    definition.bios = Some(vec![-4, 0]);
    let mut rig = Rig::new(&definition, 6);
    let before = rig.boot();
    let start = rig.sim.now();
    rig.sim.corrupt_readback(core(0));
    rig.set_all(-9).expect("set");
    let mut running = rig
        .start(basic(Regime::R1, &[0], Duration::from_secs(60)))
        .unwrap_or_else(|error| panic!("{error}"));
    rig.sim.crash();
    rig.sim.reboot().expect("a representable reboot");
    let waited = running.wait(
        &CancellationToken::new(),
        &mut crate::common::Recorder::default(),
    );
    assert!(matches!(
        waited,
        Err(Partial {
            error: Error::Crashed,
            ..
        })
    ));
    assert_ne!(before, rig.boot());
    assert_eq!(rig.read(0).expect("read"), ObservedOffset(-4));
    assert_eq!(rig.read(1).expect("read"), ObservedOffset(0));
    assert_eq!(
        rig.sim.now().duration_since(start),
        SignedDuration::from_secs(90)
    );
    assert_eq!(rig.sim.monotonic(), Duration::ZERO);
}

#[test]
fn a_cancelled_wait_does_not_advance_the_clock() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 6);
    let mut running = rig
        .start(basic(Regime::R1, &[0], Duration::from_secs(60)))
        .unwrap_or_else(|error| panic!("{error}"));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let start = rig.sim.now();
    let waited = running.wait(&cancel, &mut crate::common::Recorder::default());
    assert!(matches!(
        waited,
        Err(Partial {
            error: Error::Cancelled,
            ..
        })
    ));
    assert_eq!(rig.sim.now(), start);
}

#[test]
fn a_cancelled_wait_consumes_the_trial() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 6);
    let mut running = rig
        .start(basic(Regime::R1, &[0], Duration::from_secs(60)))
        .unwrap_or_else(|error| panic!("{error}"));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut recorder = crate::common::Recorder::default();
    assert!(running.wait(&cancel, &mut recorder).is_err());
    let start = rig.sim.now();
    let again = running.wait(&CancellationToken::new(), &mut recorder);
    assert!(matches!(
        again,
        Err(Partial {
            error: Error::Other(_),
            ..
        })
    ));
    assert_eq!(rig.sim.now(), start, "the second wait ran nothing");
    assert!(running.stop().is_ok());
}

#[test]
fn a_trial_is_waited_on_once() {
    let mut rig = Rig::new(&two_cores(ModelDef::default()), 6);
    let mut running = rig
        .start(basic(Regime::R1, &[0], Duration::from_secs(60)))
        .unwrap_or_else(|error| panic!("{error}"));
    let mut recorder = crate::common::Recorder::default();
    running
        .wait(&CancellationToken::new(), &mut recorder)
        .expect("the first wait");
    let now = rig.sim.now();
    let again = running.wait(&CancellationToken::new(), &mut recorder);
    assert!(matches!(
        again,
        Err(Partial {
            error: Error::Other(_),
            ..
        })
    ));
    assert_eq!(rig.sim.now(), now, "the second wait ran nothing");
    assert!(running.stop().is_ok());
}

#[test]
fn signal_weights_pick_in_proportion_and_skip_zero_weights() {
    let model_with = |weights: &[(Signal, f64)]| {
        let mut definition = machine(2, vec![]);
        definition.model.signals = Weights(weights.iter().copied().collect());
        model(&definition)
    };
    assert_eq!(
        model_with(&[(Signal::Crash, 0.25)])
            .signal_mix(Regime::R1)
            .pick(0.5),
        Signal::Crash
    );
    assert_eq!(
        model_with(&[(Signal::Crash, 0.0), (Signal::Stall, 0.25)])
            .signal_mix(Regime::R1)
            .pick(0.5),
        Signal::Stall
    );
    let even = model_with(&[(Signal::Stall, 1.0), (Signal::Crash, 1.0)]);
    let mix = even.signal_mix(Regime::R1);
    assert_eq!(mix.pick(0.0), Signal::Stall);
    assert_eq!(mix.pick(0.49), Signal::Stall);
    assert_eq!(mix.pick(0.5), Signal::Crash);
    assert_eq!(mix.pick(0.999), Signal::Crash);
}

#[test]
fn the_simulator_continues_after_its_history() {
    let last = jiff::Timestamp::from_second(1_800_000_000).expect("a time");
    let history = History {
        boots: 7,
        last_time: Some(last),
        bios_context: Some(BiosContext {
            bios_version: "RECORDED".to_owned(),
            ..BiosContext::default()
        }),
    };
    let settings = RunSettings {
        seed: 1,
        history,
        replay: None,
    };
    let mut rig = Rig::with(model(&machine(2, vec![])), settings);
    assert_eq!(
        rig.sim.now().duration_since(last),
        SignedDuration::from_secs(90)
    );
    assert_eq!(
        rig.machine
            .host
            .bios_context()
            .expect("a context")
            .bios_version,
        "RECORDED",
        "a machine file that declares no context follows the history"
    );
    let first = rig.boot();
    let fresh = Rig::new(&machine(2, vec![]), 1).boot();
    assert_ne!(first, fresh, "boot IDs continue from the recorded boots");
    let declared = {
        let mut definition = machine(2, vec![]);
        definition.bios_context = Some(togi_sim::definition::BiosContextDef {
            bios_version: Some("DECLARED".to_owned()),
            ..Default::default()
        });
        definition
    };
    let settings = RunSettings {
        seed: 1,
        history: History {
            bios_context: Some(BiosContext::default()),
            ..History::default()
        },
        replay: None,
    };
    let mut rig = Rig::with(model(&declared), settings);
    assert_eq!(
        rig.machine
            .host
            .bios_context()
            .expect("a context")
            .bios_version,
        "DECLARED"
    );
}

#[test]
fn a_simulator_can_be_built_without_run_settings() {
    let simulator = Simulator::new(model(&machine(2, vec![])), RunSettings::default())
        .expect("a representable machine");
    let mut machine = simulator.seams();
    assert!(machine.host.boot_id().is_ok());
}

#[test]
fn a_restored_machine_reports_what_the_crashed_one_left_for_its_next_boot() {
    let definition = two_cores(ModelDef {
        crash_mce: 1.0,
        ..sharp(Signal::Crash)
    });
    let mut rig = Rig::new(&definition, 1);
    let crashed_boot = rig.boot();
    assert!(crashed(&rig.alone(0, -12, Regime::R2, 0)));
    let text = serde_json::to_string(&rig.sim.persisted()).expect("an encoded machine");
    let restored = Simulator::restore(
        model(&definition),
        None,
        serde_json::from_str(&text).expect("a decoded machine"),
    )
    .expect("a restored machine");
    assert_eq!(restored.persisted(), rig.sim.persisted());

    let next_boot = |sim: &Simulator| {
        sim.reboot().expect("a representable reboot");
        let mut machine = sim.seams();
        let boot = machine.host.boot_id().expect("a booted machine");
        let reason = machine.kernel.reset_reason(&boot).expect("a reason");
        let after = machine
            .kernel
            .reset_reason_after(&crashed_boot)
            .expect("the reason after the crashed boot");
        let mces = machine.kernel.mces(&boot, Duration::ZERO).expect("a log");
        (boot, reason, after, mces, sim.now(), sim.monotonic())
    };
    let want = next_boot(&rig.sim);
    assert_eq!(next_boot(&restored), want);
    assert_ne!(want.0, crashed_boot);
    assert!(want.1.kind.is_some(), "{:?}", want.1);
    assert!(
        want.3
            .iter()
            .any(|mce| !mce.corrected && mce.core == Some(core(0))),
        "{:?}",
        want.3
    );
}

#[test]
fn a_persisted_machine_for_other_cores_is_refused() {
    let rig = Rig::new(&two_cores(ModelDef::default()), 1);
    let error = Simulator::restore(model(&machine(4, vec![])), None, rig.sim.persisted())
        .err()
        .expect("a refusal");
    assert!(matches!(
        error,
        togi_sim::RestoreError::Cores {
            persisted: 2,
            model: 4
        }
    ));
}

#[test]
fn a_persisted_machine_without_boots_is_refused() {
    let definition = two_cores(ModelDef::default());
    let rig = Rig::new(&definition, 1);
    let mut value = serde_json::to_value(rig.sim.persisted()).expect("an encoded machine");
    let boots = value
        .get_mut("boots")
        .and_then(serde_json::Value::as_array_mut)
        .expect("a boots array");
    assert!(!boots.is_empty());
    boots.clear();
    let persisted = serde_json::from_value(value).expect("a decoded machine");
    let error = Simulator::restore(model(&definition), None, persisted)
        .err()
        .expect("a refusal");
    assert!(matches!(error, togi_sim::RestoreError::NoBoot));
}

fn positional(object: &serde_json::Value, fields: &[&str]) -> serde_json::Value {
    let map = object.as_object().expect("an object");
    assert_eq!(map.len(), fields.len(), "{object}");
    serde_json::Value::Array(fields.iter().map(|field| map[*field].clone()).collect())
}

fn refuses_positional(machine: &serde_json::Value, pointer: &str, fields: &[&str]) {
    serde_json::from_value::<togi_sim::Persisted>(machine.clone()).expect("a decoded machine");
    let mut value = machine.clone();
    let target = value
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("{pointer} in {machine}"));
    let array = positional(target, fields);
    *target = array;
    assert!(
        serde_json::from_value::<togi_sim::Persisted>(value).is_err(),
        "{pointer} decoded from a positional array"
    );
}

#[test]
fn a_persisted_machine_decodes_only_from_objects() {
    let definition = two_cores(ModelDef {
        crash_mce: 1.0,
        ..sharp(Signal::Crash)
    });
    let mut rig = Rig::new(&definition, 1);
    rig.boot();
    assert!(crashed(&rig.alone(0, -12, Regime::R2, 0)));
    let mut crashed_machine =
        serde_json::to_value(rig.sim.persisted()).expect("an encoded machine");
    refuses_positional(
        &crashed_machine,
        "",
        &[
            "seed",
            "epoch",
            "clock",
            "boot_at",
            "wall_offset",
            "boot_number",
            "boots",
            "regs",
            "crashed",
            "queued",
            "next_reset",
            "bios",
            "samples",
        ],
    );
    refuses_positional(&crashed_machine, "/boots/0", &["id", "reason", "mces"]);
    refuses_positional(
        &crashed_machine,
        "/boots/0/reason",
        &["kind", "raw", "supported"],
    );
    refuses_positional(
        &crashed_machine,
        "/queued/0",
        &["core", "bank", "bank_type", "corrected"],
    );
    refuses_positional(
        &crashed_machine,
        "/samples",
        &["id", "cores", "threads", "ran", "stalled", "voltage"],
    );
    crashed_machine["samples"]["voltage"] = serde_json::json!({
        "requests": vec![1.0; 16],
        "clocks": [4000, 0],
        "rail": 1.0,
        "rates": vec![0.0; 16],
    });
    refuses_positional(
        &crashed_machine,
        "/samples/voltage",
        &["requests", "clocks", "rail", "rates"],
    );

    rig.sim.reboot().expect("a representable reboot");
    let rebooted = serde_json::to_value(rig.sim.persisted()).expect("an encoded machine");
    let last = rebooted["boots"].as_array().expect("boots").len() - 1;
    refuses_positional(
        &rebooted,
        &format!("/boots/{last}/mces/0"),
        &[
            "cpu",
            "core",
            "bank",
            "bank_type",
            "corrected",
            "time",
            "monotonic",
            "lines",
        ],
    );
}

#[test_case(jiff::Timestamp::MAX, SignedDuration::from_secs(1); "upper limit")]
#[test_case(jiff::Timestamp::MIN, SignedDuration::from_secs(-1); "lower limit")]
fn restoring_an_unrepresentable_wall_time_is_refused(
    epoch: jiff::Timestamp,
    offset: SignedDuration,
) {
    let definition = machine(2, vec![]);
    let rig = Rig::new(&definition, 1);
    let mut value = serde_json::to_value(rig.sim.persisted()).expect("a persisted machine");
    value["epoch"] = serde_json::to_value(epoch).expect("an epoch");
    value["wall_offset"] = serde_json::to_value(offset).expect("an offset");
    let persisted = serde_json::from_value(value).expect("a decoded machine");
    assert!(
        Simulator::restore(model(&definition), None, persisted).is_err(),
        "an unrepresentable wall clock must not become the epoch"
    );
}

#[test]
fn sleep_refuses_advancing_past_the_timestamp_limit() {
    let settings = RunSettings {
        history: History {
            last_time: Some(jiff::Timestamp::MAX - SignedDuration::from_secs(90)),
            ..History::default()
        },
        ..RunSettings::default()
    };
    let mut rig = Rig::with(model(&machine(2, vec![])), settings);
    let before = rig.sim.persisted();
    assert!(
        rig.machine
            .clock
            .sleep(&CancellationToken::new(), Duration::from_nanos(1))
            .is_err(),
        "an unrepresentable advance must be refused"
    );
    assert_eq!(rig.sim.persisted(), before, "a refusal preserves the clock");
}

#[test]
fn allocating_a_boot_after_the_last_recorded_number_is_refused() {
    let result = Simulator::new(
        model(&machine(2, vec![])),
        RunSettings {
            history: History {
                boots: u32::MAX,
                ..History::default()
            },
            ..RunSettings::default()
        },
    );
    assert!(matches!(result, Err(togi_sim::ArithmeticError::BootNumber)));
}

#[test]
fn rebooting_after_the_last_boot_number_is_refused() {
    let rig = Rig::with(
        model(&machine(2, vec![])),
        RunSettings {
            history: History {
                boots: u32::MAX - 1,
                ..History::default()
            },
            ..RunSettings::default()
        },
    );
    let before = rig.sim.persisted();
    assert!(matches!(
        rig.sim.reboot(),
        Err(togi_sim::ArithmeticError::BootNumber)
    ));
    assert_eq!(
        rig.sim.persisted(),
        before,
        "a refusal preserves the machine"
    );
}

#[test]
fn reboot_refuses_an_unrepresentable_wall_time() {
    let rig = Rig::with(
        model(&machine(2, vec![])),
        RunSettings {
            history: History {
                last_time: Some(jiff::Timestamp::MAX - SignedDuration::from_secs(90)),
                ..History::default()
            },
            ..RunSettings::default()
        },
    );
    let before = rig.sim.persisted();
    assert!(matches!(
        rig.sim.reboot(),
        Err(togi_sim::ArithmeticError::WallElapsed(_))
    ));
    assert_eq!(
        rig.sim.persisted(),
        before,
        "a refused reboot preserves the machine"
    );
}

#[test_case(jiff::Timestamp::MIN, true; "lower limit")]
#[test_case(jiff::Timestamp::MAX - SignedDuration::from_secs(90), true; "last resumable timestamp")]
#[test_case(jiff::Timestamp::MAX - SignedDuration::from_secs(90) + SignedDuration::from_nanos(1), false; "first overflowing reboot timestamp")]
#[test_case(jiff::Timestamp::MAX, false; "upper limit")]
fn recorded_time_requires_a_representable_reboot(last: jiff::Timestamp, accepted: bool) {
    let result = Simulator::new(
        model(&machine(2, vec![])),
        RunSettings {
            history: History {
                last_time: Some(last),
                ..History::default()
            },
            ..RunSettings::default()
        },
    );
    match result {
        Ok(sim) => {
            assert!(accepted);
            assert_eq!(sim.now(), last + SignedDuration::from_secs(90));
        }
        Err(error) => {
            assert!(!accepted);
            assert!(matches!(error, togi_sim::ArithmeticError::ResumeTime(_)));
        }
    }
}

#[test_case(jiff::Timestamp::MIN, SignedDuration::from_nanos(-1); "lower limit")]
#[test_case(jiff::Timestamp::MAX, SignedDuration::from_nanos(1); "upper limit")]
fn wall_clock_jumps_refuse_timestamp_overflow(epoch: jiff::Timestamp, by: SignedDuration) {
    let definition = machine(2, vec![]);
    let rig = Rig::new(&definition, 1);
    let mut value = serde_json::to_value(rig.sim.persisted()).expect("a persisted machine");
    value["epoch"] = serde_json::to_value(epoch).expect("an epoch");
    let sim = Simulator::restore(
        model(&definition),
        None,
        serde_json::from_value(value).expect("a machine"),
    )
    .expect("a timestamp at the limit is valid");
    let before = sim.persisted();
    assert!(matches!(
        sim.jump_wall(by),
        Err(togi_sim::ArithmeticError::WallOffset(_))
    ));
    assert_eq!(sim.persisted(), before);
    assert_eq!(sim.now(), epoch);
}

#[test]
fn trial_time_overflow_reaches_the_wait_caller() {
    let mut rig = Rig::with(
        model(&machine(2, vec![])),
        RunSettings {
            history: History {
                last_time: Some(jiff::Timestamp::MAX - SignedDuration::from_secs(90)),
                ..History::default()
            },
            ..RunSettings::default()
        },
    );
    let before = rig.sim.now();
    let error = rig
        .run(basic(Regime::R1, &[0], Duration::from_secs(1)))
        .0
        .expect_err("a trial cannot advance past the timestamp limit")
        .error;
    let Error::Other(error) = error else {
        panic!("expected an arithmetic error");
    };
    assert!(matches!(
        error.downcast_ref::<togi_sim::ArithmeticError>(),
        Some(togi_sim::ArithmeticError::WallElapsed(_))
    ));
    assert_eq!(rig.sim.now(), before);
}
