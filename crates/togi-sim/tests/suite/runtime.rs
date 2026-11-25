// Trial outcomes, determinism, faults and reboots through the seams.

use jiff::SignedDuration;
use std::time::Duration;
use togi_machine::{
    BankType, BiosContext, CancellationToken, Check, CpuId, Error, ObservedOffset, Partial, Regime,
    ResetKind, Signal, TargetOffset, TrialSpec, pick_workload,
};
use togi_sim::{
    DEFAULT_BIOS_VERSION, DEFAULT_BOARD, DEFAULT_BOOST_LIMIT_MHZ, DEFAULT_MICROCODE, History,
    ScriptedMachine, ScriptedMce, ScriptedTopology, ScriptedVoltageSamples, Simulator,
    TrialOutcome,
};

use test_case::test_case;

use crate::common::{Rig, basic, core, crashed, spec};

const NINETY: Duration = Duration::from_secs(90);

#[test]
fn r6_progress_counts_the_bursts_of_the_whole_trial() {
    let mut rig = Rig::scripted(ScriptedTopology::SixteenCores, |_, _| TrialOutcome::Pass);
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
    let mut rig = Rig::scripted(ScriptedTopology::SixteenCores, |_, _| {
        TrialOutcome::UnexpectedExit {
            core: core(0),
            at: Duration::from_secs(1),
        }
    });
    rig.set(0, -1).expect("set");
    let (waited, recorder) = rig.run(basic(Regime::R6, &[0], NINETY));
    let result = waited.expect("a result");
    assert!(result.ran < NINETY / 2, "failed at {:?}", result.ran);
    assert_eq!(recorder.progress, ["bursts: 0 continues, 1 stops"]);
}

#[test_case(false; "fresh")]
#[test_case(true; "recorded history")]
fn the_same_seed_and_history_make_the_same_observations(resumed: bool) {
    let history = if resumed {
        History {
            boots: 5,
            last_time: Some(jiff::Timestamp::from_second(1_800_000_000).expect("a time")),
            bios_context: None,
        }
    } else {
        History::default()
    };
    let run = |seed| {
        let sim = Simulator::scripted(passing_script(), seed, history.clone())
            .expect("a scripted machine");
        let mut rig = Rig::from_simulator(sim);
        let boot = rig.boot();
        let waited = rig.alone(0, -45, Regime::R1, 0);
        (boot, format!("{waited:?}"), rig.sim.now())
    };
    assert_eq!(run(2), run(2));
    assert_ne!(run(2).0, run(3).0);
}

#[test_case(
    |_, _| TrialOutcome::ComputationError { core: core(0), at: Duration::from_secs(1) },
    Signal::ComputationError;
    "computation error"
)]
#[test_case(
    |_, _| TrialOutcome::Stall { core: core(0), at: Duration::from_secs(1) },
    Signal::Stall;
    "stall"
)]
#[test_case(
    |_, _| TrialOutcome::UnexpectedExit { core: core(0), at: Duration::from_secs(1) },
    Signal::UnexpectedExit;
    "unexpected exit"
)]
#[test_case(
    |_, _| TrialOutcome::CorrectedMce {
        mce: ScriptedMce { core: core(0), bank: 0, bank_type: BankType::LoadStore },
        at: Duration::from_secs(1),
    },
    Signal::CorrectedMce;
    "corrected machine check"
)]
#[test_case(
    |_, _| TrialOutcome::ResetWithMce {
        mce: ScriptedMce { core: core(0), bank: 0, bank_type: BankType::LoadStore },
        stalled: Some(core(0)),
        kind: ResetKind::Watchdog,
        at: Duration::from_secs(1),
    },
    Signal::Crash;
    "crash"
)]
fn each_signal_has_its_result_shape(
    outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
    signal: Signal,
) {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, outcome);
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
        Signal::UncorrectedMce => unreachable!("the crash case logs an uncorrected check"),
    }
}

#[test]
fn a_readback_can_be_corrupted_once_after_the_next_write() {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
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
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
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
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
    rig.sim.fail_setup(2);
    for attempt in 0..3 {
        let started = rig.start(basic(Regime::R1, &[0], NINETY));
        assert_eq!(started.is_err(), attempt < 2, "attempt {attempt}");
    }
}

#[test]
fn a_worker_escapes_its_cpus_in_one_trial() {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
    rig.sim.escape();
    assert_eq!(
        rig.pass(basic(Regime::R1, &[1], NINETY)).escaped,
        [CpuId::from(5)]
    );
    assert!(rig.pass(basic(Regime::R1, &[1], NINETY)).escaped.is_empty());
}

#[test]
fn a_failed_check_shows_in_the_preflight_until_cleared() {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
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
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
    let changed = BiosContext {
        bios_version: "SIM.2".to_owned(),
        ..rig.machine.host.bios_context().expect("a context")
    };
    rig.sim.set_bios_context(changed.clone());
    assert_eq!(rig.machine.host.bios_context().expect("a context"), changed);
}

#[test]
fn a_crash_before_apply_takes_the_first_write_of_the_boot() {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
    rig.sim.crash_before_apply(1);
    rig.read(0).expect("a read before the first write");
    assert!(matches!(rig.set_all(0), Err(Error::Crashed)));
    rig.sim.reboot().expect("a representable reboot");
    rig.set_all(0).expect("the first write of the next boot");
}

#[test]
fn a_trial_alone_with_an_outsider_at_a_nonzero_offset_is_a_violation() {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
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
    let inputs = ScriptedMachine::new(ScriptedTopology::TwoCores, TargetOffset::ZERO, |_, _| {
        TrialOutcome::Pass
    })
    .with_boot_offsets(|id| {
        if id == core(0) {
            crate::common::offset(-4)
        } else {
            TargetOffset::ZERO
        }
    });
    let mut rig = Rig::with_script(inputs, History::default());
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
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
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
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
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
    assert!(running.stop(&mut rig.journal).is_ok());
}

#[test]
fn a_trial_is_waited_on_once() {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
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
    assert!(running.stop(&mut rig.journal).is_ok());
}

#[test]
fn a_scripted_machine_continues_after_its_history() {
    let last = jiff::Timestamp::from_second(1_800_000_000).expect("a time");
    let history = History {
        boots: 7,
        last_time: Some(last),
        bios_context: Some(BiosContext {
            bios_version: "RECORDED".to_owned(),
            ..BiosContext::default()
        }),
    };
    let mut rig = Rig::with_script(passing_script(), history);
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
        "RECORDED"
    );
    let fresh = Rig::with_script(passing_script(), History::default()).boot();
    assert_ne!(
        rig.boot(),
        fresh,
        "boot IDs continue from the recorded boots"
    );
}

#[test]
fn a_fresh_scripted_machine_has_the_default_bios_context() {
    let mut rig = Rig::scripted(ScriptedTopology::SixteenCores, |_, _| TrialOutcome::Pass);
    assert_eq!(
        (
            DEFAULT_BIOS_VERSION,
            DEFAULT_BOARD,
            DEFAULT_MICROCODE,
            DEFAULT_BOOST_LIMIT_MHZ
        ),
        ("SIM.1", "togi simulator", "0x0", 5700)
    );
    assert_eq!(
        rig.machine.host.bios_context().expect("a context"),
        BiosContext {
            bios_version: DEFAULT_BIOS_VERSION.into(),
            board: DEFAULT_BOARD.into(),
            cpu_model: "Simulated Zen 5 16-Core Processor".into(),
            microcode: DEFAULT_MICROCODE.into(),
            boost_limit_mhz: DEFAULT_BOOST_LIMIT_MHZ,
        }
    );
}

#[test]
fn a_restored_machine_reports_what_the_crashed_one_left_for_its_next_boot() {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| {
        TrialOutcome::ResetWithMce {
            mce: ScriptedMce {
                core: core(0),
                bank: 0,
                bank_type: BankType::LoadStore,
            },
            stalled: Some(core(0)),
            kind: ResetKind::Watchdog,
            at: Duration::from_secs(7),
        }
    });
    let crashed_boot = rig.boot();
    assert!(crashed(&rig.alone(0, -12, Regime::R2, 0)));
    let text = serde_json::to_string(&rig.sim.persisted()).expect("an encoded machine");
    let restored = Simulator::restore(
        rig.sim.source(),
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
    let rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
    let other = Rig::scripted(ScriptedTopology::FourCores, |_, _| TrialOutcome::Pass);
    let error = Simulator::restore(other.sim.source(), rig.sim.persisted())
        .err()
        .expect("a refusal");
    assert!(matches!(
        error,
        togi_sim::RestoreError::Cores {
            persisted: 2,
            script: 4
        }
    ));
}

#[test]
fn a_persisted_machine_without_boots_is_refused() {
    let rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
    let mut value = serde_json::to_value(rig.sim.persisted()).expect("an encoded machine");
    let boots = value
        .get_mut("boots")
        .and_then(serde_json::Value::as_array_mut)
        .expect("a boots array");
    assert!(!boots.is_empty());
    boots.clear();
    let persisted = serde_json::from_value(value).expect("a decoded machine");
    let error = Simulator::restore(rig.sim.source(), persisted)
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
fn a_scripted_persisted_machine_decodes_only_from_objects() {
    static LANES: ScriptedVoltageSamples = ScriptedVoltageSamples {
        requests_v: [1.0; 16],
        ccd_mhz: [4000, 4100],
    };
    let inputs = ScriptedMachine::sixteen_with_voltage_samples(
        TargetOffset::ZERO,
        |_, _| TrialOutcome::UncorrectedMce {
            core: core(0),
            at: Duration::from_secs(7),
        },
        &LANES,
    );
    let mut rig = Rig::with_script(inputs, History::default());
    rig.boot();
    assert!(crashed(&rig.alone(0, -12, Regime::R2, 0)));
    let crashed_machine = serde_json::to_value(rig.sim.persisted()).expect("an encoded machine");
    refuses_positional(
        &crashed_machine,
        "",
        &[
            "source",
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
    refuses_positional(
        &crashed_machine,
        "/samples/voltage",
        &["requests", "clocks"],
    );
    let mut legacy_voltage = crashed_machine;
    legacy_voltage["samples"]["voltage"]["rail"] = serde_json::json!(1.0);
    legacy_voltage["samples"]["voltage"]["rates"] = serde_json::json!([0.0; 16]);
    assert!(serde_json::from_value::<togi_sim::Persisted>(legacy_voltage).is_err());

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
    let rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
    let mut value = serde_json::to_value(rig.sim.persisted()).expect("a persisted machine");
    value["epoch"] = serde_json::to_value(epoch).expect("an epoch");
    value["wall_offset"] = serde_json::to_value(offset).expect("an offset");
    let persisted = serde_json::from_value(value).expect("a decoded machine");
    assert!(
        Simulator::restore(rig.sim.source(), persisted).is_err(),
        "an unrepresentable wall clock must not become the epoch"
    );
}

#[test]
fn sleep_refuses_advancing_past_the_timestamp_limit() {
    let mut rig = Rig::with_script(
        passing_script(),
        History {
            last_time: Some(jiff::Timestamp::MAX - SignedDuration::from_secs(90)),
            ..History::default()
        },
    );
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
    let result = Simulator::scripted(
        passing_script(),
        1,
        History {
            boots: u32::MAX,
            ..History::default()
        },
    );
    assert!(matches!(result, Err(togi_sim::ArithmeticError::BootNumber)));
}

#[test]
fn rebooting_after_the_last_boot_number_is_refused() {
    let rig = Rig::with_script(
        passing_script(),
        History {
            boots: u32::MAX - 1,
            ..History::default()
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
    let rig = Rig::with_script(
        passing_script(),
        History {
            last_time: Some(jiff::Timestamp::MAX - SignedDuration::from_secs(90)),
            ..History::default()
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
    let result = Simulator::scripted(
        passing_script(),
        1,
        History {
            last_time: Some(last),
            ..History::default()
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
    let rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Pass);
    let mut value = serde_json::to_value(rig.sim.persisted()).expect("a persisted machine");
    value["epoch"] = serde_json::to_value(epoch).expect("an epoch");
    let sim = Simulator::restore(
        rig.sim.source(),
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
    let mut rig = Rig::with_script(
        passing_script(),
        History {
            last_time: Some(jiff::Timestamp::MAX - SignedDuration::from_secs(90)),
            ..History::default()
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

#[test]
fn a_corrected_mce_trial_refused_at_its_end_leaves_no_future_kernel_event() {
    let inputs = ScriptedMachine::new(ScriptedTopology::TwoCores, TargetOffset::ZERO, |_, _| {
        TrialOutcome::CorrectedMce {
            mce: ScriptedMce {
                core: core(0),
                bank: 0,
                bank_type: BankType::LoadStore,
            },
            at: Duration::from_millis(500),
        }
    });
    let mut rig = Rig::with_script(
        inputs,
        History {
            last_time: Some(jiff::Timestamp::MAX - SignedDuration::from_secs(91)),
            ..History::default()
        },
    );
    let before = rig.sim.persisted();
    let error = rig
        .run(basic(Regime::R1, &[0], Duration::from_secs(2)))
        .0
        .expect_err("the trial end is outside the timestamp range")
        .error;
    let Error::Other(error) = error else {
        panic!("expected an arithmetic error");
    };
    assert!(matches!(
        error.downcast_ref::<togi_sim::ArithmeticError>(),
        Some(togi_sim::ArithmeticError::WallElapsed(_))
    ));
    assert_eq!(
        rig.sim.persisted(),
        before,
        "a refused trial leaves no MCE ahead of its clock"
    );
}

fn passing_script() -> ScriptedMachine {
    ScriptedMachine::new(ScriptedTopology::TwoCores, TargetOffset::ZERO, |_, _| {
        TrialOutcome::Pass
    })
}
