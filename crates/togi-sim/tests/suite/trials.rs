// What a trial's outcome depends on: limits, hazards, scripts, replay facts and reset draws.

use jiff::SignedDuration;
use std::time::Duration;
use togi_machine::{
    BiosContext, CancellationToken, Condition, Error, Regime, ResetKind, Signal, pick_workload,
};
use togi_sim::definition::{BiosContextDef, CcdDef, JointDef, ModelDef, ScriptDef, Weights};
use togi_sim::model::{Onset, Script};
use togi_sim::replay::{Replay, ReplayFact, ReplayOutcome};
use togi_sim::{MachineDefinition, RunSettings, ScriptError};

use test_case::test_case;

use crate::common::{
    Rig, basic, core, cores, crashed, flat_cores, machine, model, offset, offsets, sharp, spec,
};

const SECOND: Duration = Duration::from_secs(1);
const MINUTE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
enum Mixes {
    None,
    R7Computation,
    R2Computation,
}
#[derive(Clone, Copy)]
enum Source {
    Joint {
        signal: Option<Signal>,
        members: [i64; 2],
    },
    Ccd,
}

type ScriptRow = (&'static str, Signal, f64, i64, Option<ResetKind>, bool);

fn joint(members: &[(i64, i64)], rate: f64) -> JointDef {
    JointDef {
        members: members.iter().copied().collect(),
        rate: Some(rate),
        ..JointDef::default()
    }
}

fn with(
    cores_count: i64,
    bios: &[i64],
    limits: (i64, i64),
    model: ModelDef,
    joints: Vec<JointDef>,
) -> MachineDefinition {
    let mut definition = machine(cores_count, flat_cores(cores_count, limits.0, limits.1));
    definition.bios = Some(bios.to_vec());
    definition.model = model;
    definition.joint = joints;
    definition
}

fn failure_of(waited: &crate::common::Waited) -> Option<(Signal, i64)> {
    waited
        .as_ref()
        .ok()
        .and_then(|result| result.failure)
        .map(|failure| (failure.signal, i64::from(failure.core.get())))
}

#[test]
fn a_retry_reproduces_the_class_outcome() {
    let definition = with(
        2,
        &[-11, 0],
        (-10, -10),
        ModelDef {
            past_limit_rate: 0.1,
            ..sharp(Signal::ComputationError)
        },
        vec![],
    );
    let mut rig = Rig::new(&definition, 42);
    let mut trial = |id: &str, index: usize, condition: Condition| {
        let mut spec = spec(id, Regime::R1, pick_workload(Regime::R1, 0), &[0], MINUTE);
        spec.index = index;
        spec.condition = condition;
        rig.pass(spec).ran
    };
    let first = trial("0008", 7, Condition::Together);
    assert!(first < MINUTE, "the seed fails inside the trial");
    assert_eq!(
        first,
        trial("0009", 7, Condition::Together),
        "the trial ID is not part of the draw"
    );
    assert_ne!(first, trial("0010", 8, Condition::Together), "the index is");
    assert_ne!(
        first,
        trial("0011", 7, Condition::Alone),
        "so is the condition"
    );
    assert_eq!(
        trial("0012", 7, Condition::Parked),
        trial("0013", 7, Condition::Parked),
        "a parked retry reproduces the outcome"
    );
    let parked = trial("0014", 7, Condition::Parked);
    assert_ne!(parked, first, "parked is not together");
    assert_ne!(
        parked,
        trial("0015", 7, Condition::Alone),
        "parked is not alone"
    );
}

fn failure_time(boost: f64, onset_s: f64, rate: f64, growth: f64, duration: Duration) -> Duration {
    let definition = with(
        2,
        &[-11, 0],
        (-10, -10),
        ModelDef {
            past_limit_rate: rate,
            growth,
            onset_s,
            onset_boost: boost,
            ..sharp(Signal::ComputationError)
        },
        vec![],
    );
    let mut rig = Rig::new(&definition, 42);
    let result = rig.pass(basic(Regime::R1, &[0], duration));
    assert_eq!(
        result.failure.map(|failure| failure.signal),
        Some(Signal::ComputationError)
    );
    result.ran
}

#[test_case(2.0; "a small boost")]
#[test_case(9.0; "a large boost")]
#[test_case(1000.0; "a huge boost")]
fn an_onset_boost_moves_the_failure_along_the_same_cumulative_hazard(boost: f64) {
    let rate = 0.1;
    let steady = failure_time(0.0, 10.0, rate, 4.0, MINUTE);
    let boosted = failure_time(boost, 10.0, rate, 4.0, MINUTE);
    assert!(boosted <= steady);
    let onset = Onset {
        window_s: 10.0,
        boost,
    };
    let drawn = rate * steady.as_secs_f64();
    let integrated = rate * onset.exposure(0.0, boosted.as_secs_f64());
    assert!((drawn - integrated).abs() < 1e-6, "{drawn} vs {integrated}");
}

#[test]
fn a_failure_after_the_onset_window_keeps_its_exposure() {
    let day = Duration::from_hours(24);
    let steady = failure_time(0.0, 1.0, 0.001, 1.0, day);
    let boosted = failure_time(2.0, 1.0, 0.001, 1.0, day);
    assert!(boosted > SECOND, "the seed reaches post-onset exposure");
    let onset = Onset {
        window_s: 1.0,
        boost: 2.0,
    };
    let drawn = 0.001 * steady.as_secs_f64();
    let integrated = 0.001 * onset.exposure(0.0, boosted.as_secs_f64());
    assert!((drawn - integrated).abs() < 1e-6, "{drawn} vs {integrated}");
}

#[test]
fn a_joint_fails_only_in_its_regimes_and_after_its_delay() {
    let mut delayed = joint(&[(0, -20), (1, -20)], 1e6);
    delayed.regimes = vec![Regime::R7];
    delayed.after_s = 30.0;
    let definition = with(
        2,
        &[-20, -20],
        (-30, -30),
        sharp(Signal::Crash),
        vec![delayed],
    );
    let mut rig = Rig::new(&definition, 1);
    let mut trial = |regime, seconds| {
        let mut spec = basic(regime, &[0], Duration::from_secs(seconds));
        spec.id = format!("{seconds}");
        rig.run(spec).0
    };
    assert!(trial(Regime::R1, 60).is_ok(), "another regime");
    assert!(trial(Regime::R7, 20).is_ok(), "before the delay");
    assert!(crashed(&trial(Regime::R7, 60)));
    assert!(rig.sim.monotonic() >= Duration::from_secs(110));
}

#[test]
fn a_joint_failure_is_attributed_to_the_lowest_loaded_member_whatever_the_order() {
    let mut named = joint(&[(0, -20), (1, -20)], 1e6);
    named.signal = Some(Signal::ComputationError);
    let definition = with(
        2,
        &[-20, -20],
        (-30, -30),
        sharp(Signal::Crash),
        vec![named],
    );
    let attributed = |loaded: &[i64]| {
        let mut rig = Rig::new(&definition, 1);
        failure_of(&rig.run(basic(Regime::R7, loaded, SECOND)).0)
    };
    assert_eq!(attributed(&[1, 0]), Some((Signal::ComputationError, 0)));
    assert_eq!(attributed(&[0, 1]), Some((Signal::ComputationError, 0)));
    assert_eq!(attributed(&[1]), Some((Signal::ComputationError, 1)));
}

#[test]
fn a_joint_without_a_signal_crashes_whatever_the_model_signal() {
    let definition = with(
        2,
        &[-20, 0],
        (-30, -30),
        ModelDef {
            crash_mce: 0.0,
            ..sharp(Signal::ComputationError)
        },
        vec![joint(&[(0, -20)], 1e6)],
    );
    let mut rig = Rig::new(&definition, 1);
    assert!(crashed(&rig.run(basic(Regime::R1, &[0], SECOND)).0));
}

fn crash_mce_model() -> ModelDef {
    ModelDef {
        crash_mce: 1.0,
        ..sharp(Signal::ComputationError)
    }
}

#[test_case(
    || with(4, &[0, -20, -20, 0], (-30, -30), crash_mce_model(), vec![joint(&[(1, -20), (2, -20)], 1e6)]);
    "joint"
)]
#[test_case(
    || {
        let mut idle = with(2, &[0, -11], (-30, -30), crash_mce_model(), vec![]);
        idle.core[1].idle = Some(-10);
        idle
    };
    "idle"
)]
#[test_case(
    || {
        let mut flat = with(2, &[0, -1], (-30, -30), crash_mce_model(), vec![]);
        flat.core[1].flat = 1e6;
        flat
    };
    "flat"
)]
fn a_crash_of_an_unloaded_joint_or_idle_core_leaves_no_machine_check(
    definition: fn() -> MachineDefinition,
) {
    let mut rig = Rig::new(&definition(), 1);
    assert!(crashed(&rig.run(basic(Regime::R1, &[0], SECOND)).0));
    rig.sim.reboot();
    let boot = rig.boot();
    assert!(rig.mces(&boot).is_empty());
}

#[test]
fn a_workload_limit_beats_the_alone_limit_and_an_outsider_switches_to_together() {
    let mut definition = with(
        2,
        &[-15, 0],
        (-20, -10),
        sharp(Signal::ComputationError),
        vec![],
    );
    let limited = pick_workload(Regime::R1, 1);
    definition.core[0].workload = [(limited.id(), -5)].into();
    let mut rig = Rig::new(&definition, 1);
    assert!(
        rig.pass(basic(Regime::R1, &[0], SECOND)).failure.is_none(),
        "alone limit -20"
    );
    let result = rig.pass(spec("0002", Regime::R1, limited, &[0], SECOND));
    assert!(result.failure.is_some(), "workload limit -5");
    rig.set(1, -1).expect("set");
    assert!(
        rig.pass(basic(Regime::R1, &[0], SECOND)).failure.is_some(),
        "together limit -10"
    );
}

#[test]
fn a_tiny_rate_does_not_overflow_into_a_failure() {
    let mut definition = with(2, &[-1, 0], (-10, -10), ModelDef::default(), vec![]);
    definition.core[0].flat = 1e-300;
    let mut rig = Rig::new(&definition, 1);
    assert!(rig.pass(basic(Regime::R1, &[0], MINUTE)).failure.is_none());
}

#[test]
fn an_unloaded_core_draws_by_the_loaded_set_not_its_order() {
    let mut definition = with(
        4,
        &[0, 0, -5, 0],
        (-30, -30),
        ModelDef {
            near_limit_rate: 0.0,
            ..ModelDef::default()
        },
        vec![],
    );
    definition.core[2].flat = 1.0 / 60.0;
    let crash_at = |loaded: &[i64]| {
        let mut rig = Rig::new(&definition, 1);
        let day = Duration::from_hours(24);
        assert!(
            crashed(&rig.run(basic(Regime::R1, loaded, day)).0),
            "{loaded:?}"
        );
        rig.sim.monotonic()
    };
    let on_first = crash_at(&[0]);
    assert_ne!(on_first, crash_at(&[1]), "another loaded set draws again");
    assert_eq!(on_first, crash_at(&[0]), "a retry");
    assert_eq!(
        crash_at(&[0, 1]),
        crash_at(&[1, 0]),
        "the order of the loaded set"
    );
}

#[test_case(None, false; "by default")]
#[test_case(Some(0), true; "core 0")]
#[test_case(Some(1), true; "core 1")]
fn a_joint_names_a_machine_check_core_only_when_it_is_told_to(
    core_named: Option<i64>,
    expects: bool,
) {
    let mut crashing = joint(&[(0, -20), (1, -20)], 1e6);
    crashing.regimes = vec![Regime::R7];
    crashing.crash_mce_core = core_named;
    let definition = with(
        2,
        &[-20, -20],
        (-30, -30),
        ModelDef {
            crash_mce: 1.0,
            core_local_bank: 0.0,
            ..ModelDef::default()
        },
        vec![crashing],
    );
    let mut rig = Rig::new(&definition, 1);
    assert!(crashed(&rig.run(basic(Regime::R7, &[1, 0], SECOND)).0));
    rig.sim.reboot();
    let boot = rig.boot();
    let mces = rig.mces(&boot);
    if !expects {
        assert!(
            mces.is_empty(),
            "a default joint crash invented evidence: {mces:?}"
        );
        return;
    }
    let target = core_named.map(core);
    assert_eq!(mces.len(), 1);
    let mce = &mces[0];
    assert_eq!(
        (mce.core, mce.cpu.0),
        (
            target,
            u16::from(target.map_or(0, togi_machine::CoreId::get))
        )
    );
    assert_eq!(mce.bank_type, togi_machine::BankType::LoadStore);
    assert!(!mce.corrected);
}

#[test_case(0; "the first core")]
#[test_case(5; "the last core")]
fn a_misleading_machine_check_core_can_be_a_nonmember_that_is_not_loaded(named: i64) {
    const CORES: i64 = 6;
    let members = [(2, -20), (3, -20)];
    let loaded = [2, 3];
    let mut bios = [0; 6];
    bios[2] = -20;
    bios[3] = -20;
    let config = |joints: Vec<JointDef>| {
        with(
            CORES,
            &bios,
            (-30, -30),
            ModelDef {
                crash_mce: 1.0,
                ..sharp(Signal::Crash)
            },
            joints,
        )
    };
    let mut control = Rig::new(&config(vec![]), 1);
    let result = control.pass(spec(
        "0001",
        Regime::R1,
        pick_workload(Regime::R1, 0),
        &loaded,
        MINUTE,
    ));
    assert!(result.failure.is_none());

    let mut crashing = joint(&members, 1e6);
    crashing.signal = Some(Signal::Crash);
    crashing.crash_mce_core = Some(named);
    let mut rig = Rig::new(&config(vec![crashing]), 1);
    let crashed_boot = rig.boot();
    let waited = rig
        .run(spec(
            "0001",
            Regime::R1,
            pick_workload(Regime::R1, 0),
            &loaded,
            MINUTE,
        ))
        .0;
    assert!(crashed(&waited));
    assert!(matches!(rig.machine.host.boot_id(), Err(Error::Crashed)));
    rig.sim.reboot();
    let boot = rig.boot();
    assert_ne!(boot, crashed_boot);
    assert!(rig.mces(&crashed_boot).is_empty());
    let mces = rig.mces(&boot);
    assert_eq!(mces.len(), 1);
    let want = core(named);
    assert_eq!(
        (mces[0].core, mces[0].cpu.0),
        (Some(want), u16::from(want.get()))
    );
    assert_eq!(mces[0].bank_type, togi_machine::BankType::LoadStore);
    assert!(!mces[0].corrected);
}

fn scripted(definition: &mut MachineDefinition, scripts: &[ScriptRow]) {
    definition.script = scripts
        .iter()
        .map(
            |&(trial, signal, at_s, core, reset, then_crash)| ScriptDef {
                trial: trial.to_owned(),
                signal: Some(signal),
                at_s,
                core,
                reset,
                then_crash,
            },
        )
        .collect();
}

#[test]
fn scripts_drive_outcomes_and_the_wall_and_monotonic_clocks_apart() {
    let mut definition = machine(2, flat_cores(2, -50, -50));
    scripted(
        &mut definition,
        &[
            ("0001", Signal::CorrectedMce, 7.0, 0, None, false),
            (
                "0002",
                Signal::ComputationError,
                3.0,
                1,
                Some(ResetKind::ThermalTrip),
                true,
            ),
        ],
    );
    let mut rig = Rig::new(&definition, 1);
    let epoch = rig.sim.now();
    let boot = rig.boot();
    rig.sim.jump_wall(SignedDuration::from_hours(-1));
    let result = rig.pass(basic(Regime::R1, &[0], Duration::from_secs(10)));
    assert!(result.failure.is_none());
    let mces = rig
        .machine
        .kernel
        .mces(&boot, Duration::from_secs(7))
        .expect("a boot");
    assert_eq!(mces.len(), 1);
    assert_eq!(mces[0].monotonic, Duration::from_secs(7));
    assert_eq!(
        mces[0].time,
        epoch + SignedDuration::from_hours(-1) + SignedDuration::from_secs(7)
    );
    let later = rig
        .machine
        .kernel
        .mces(&boot, Duration::from_secs(7) + Duration::from_nanos(1))
        .expect("a boot");
    assert!(later.is_empty());

    rig.machine
        .clock
        .sleep(&CancellationToken::new(), Duration::from_secs(5))
        .expect("a sleep");
    assert_eq!(rig.sim.monotonic(), Duration::from_secs(15));
    assert_eq!(
        rig.sim.now(),
        epoch + SignedDuration::from_hours(-1) + SignedDuration::from_secs(15)
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        rig.machine.clock.sleep(&cancel, SECOND),
        Err(Error::Cancelled)
    ));
    assert_eq!(rig.sim.monotonic(), Duration::from_secs(15));

    let mut second = basic(Regime::R1, &[1], MINUTE);
    second.id = "0002".to_owned();
    let (waited, recorder) = rig.run(second);
    assert!(crashed(&waited));
    let Err(partial) = &waited else {
        panic!("a crash");
    };
    assert_eq!(partial.value.ran, Duration::from_secs(3));
    assert_eq!(rig.sim.monotonic(), Duration::from_secs(18));
    assert_eq!(recorder.signals, [(core(1), Signal::ComputationError)]);
    rig.sim.reboot();
    let boot = rig.boot();
    let reason = rig.machine.kernel.reset_reason(&boot).expect("a reason");
    assert_eq!(reason.kind, Some(ResetKind::ThermalTrip));
    assert!(
        reason
            .raw
            .iter()
            .any(|raw| raw.contains("thermal pin BP_THERMTRIP_L was tripped"))
    );
    assert_eq!(rig.sim.monotonic(), Duration::ZERO);
}

#[test_case(Signal::Crash, &[]; "a crash")]
#[test_case(Signal::ComputationError, &[(core(0), Signal::ComputationError)]; "a backend signal")]
fn a_script_that_then_crashes_reports_only_backend_signals(
    signal: Signal,
    want: &[(togi_machine::CoreId, Signal)],
) {
    let mut definition = machine(2, vec![]);
    scripted(&mut definition, &[("0001", signal, 0.0, 0, None, true)]);
    let mut rig = Rig::new(&definition, 1);
    let (waited, recorder) = rig.run(basic(Regime::R7, &[0], SECOND));
    assert!(crashed(&waited));
    assert_eq!(recorder.signals, want);
    assert!(matches!(rig.machine.host.boot_id(), Err(Error::Crashed)));
}

#[test_case(Regime::R1; "r1")]
#[test_case(Regime::R6; "r6")]
fn a_script_that_crashes_after_a_signal_returns_the_exposure_it_ran(regime: Regime) {
    let partial_of = |then_crash: bool| {
        let mut definition = machine(2, vec![]);
        scripted(
            &mut definition,
            &[("0001", Signal::Crash, 40.0, 0, None, then_crash)],
        );
        let mut rig = Rig::new(&definition, 1);
        let (waited, recorder) = rig.run(basic(regime, &[0], MINUTE));
        let Err(partial) = waited else {
            panic!("a crash");
        };
        (partial.value, recorder.progress, rig.sim.monotonic())
    };
    let (ordinary, ordinary_progress, _) = partial_of(false);
    let (chained, chained_progress, monotonic) = partial_of(true);
    assert_eq!(chained.ran, Duration::from_secs(40));
    assert_eq!(monotonic, Duration::from_secs(40));
    assert_eq!(
        (chained.stops, chained.conts),
        (ordinary.stops, ordinary.conts)
    );
    assert_eq!(chained_progress, ordinary_progress);
    if regime == Regime::R6 {
        assert!(chained.conts > 0, "the bursts before the crash are counted");
    }
}

#[test_case(1.0, true; "core local")]
#[test_case(0.0, false; "shared")]
fn a_replayed_uncorrected_machine_check_names_a_bank_by_the_models_probability(
    local: f64,
    want_local: bool,
) {
    let mut definition = machine(2, flat_cores(2, -50, -50));
    definition.bios = Some(vec![-20, -21]);
    definition.model.core_local_bank = local;
    let work = pick_workload(Regime::R7, 0);
    let mut banks = std::collections::BTreeSet::new();
    for seed in 0..60u64 {
        let model = model(&definition);
        let replay = replay_of(model.bios_context().clone(), Signal::UncorrectedMce);
        let mut rig = Rig::with(
            model,
            RunSettings {
                seed,
                replay: Some(replay),
                ..RunSettings::default()
            },
        );
        let (waited, _) = rig.run(spec("1", Regime::R7, work, &[0, 1], MINUTE));
        assert!(crashed(&waited), "seed {seed}: {waited:?}");
        rig.sim.reboot();
        let boot = rig.boot();
        let mces = rig.mces(&boot);
        assert_eq!(mces.len(), 1, "seed {seed}: {mces:?}");
        assert_eq!(mces[0].bank_type.core_local(), want_local, "seed {seed}");
        banks.insert(mces[0].bank);
    }
    assert!(banks.len() > 1, "the bank is drawn, not fixed");
}

#[test]
fn a_scripted_backend_failure_reports_and_a_scripted_crash_picks_its_reset() {
    let mut definition = machine(2, vec![]);
    scripted(
        &mut definition,
        &[
            ("0001", Signal::ComputationError, 2.0, 0, None, false),
            (
                "0002",
                Signal::Crash,
                3.0,
                0,
                Some(ResetKind::CpuShutdown),
                false,
            ),
        ],
    );
    let mut rig = Rig::new(&definition, 1);
    let (waited, recorder) = rig.run(basic(Regime::R1, &[0], MINUTE));
    let result = waited.expect("a result");
    assert_eq!(
        (result.ran, result.failure.map(|failure| failure.core)),
        (Duration::from_secs(2), Some(core(0)))
    );
    assert_eq!(recorder.signals, [(core(0), Signal::ComputationError)]);
    let mut second = basic(Regime::R1, &[0], MINUTE);
    second.id = "0002".to_owned();
    assert!(crashed(&rig.run(second).0));
    rig.sim.reboot();
    let boot = rig.boot();
    let reason = rig.machine.kernel.reset_reason(&boot).expect("a reason");
    assert_eq!(reason.kind, Some(ResetKind::CpuShutdown));
}

#[test]
fn a_trial_can_be_scripted_after_the_machine_is_built() {
    let mut rig = Rig::new(&machine(2, flat_cores(2, -20, -20)), 1);
    rig.sim
        .script_trial(
            "0001",
            Script {
                signal: Some(Signal::ComputationError),
                at_s: 2.0,
                core: core(1),
                reset: None,
                then_crash: false,
            },
        )
        .expect("a script");
    let result = rig.pass(basic(Regime::R1, &[1], MINUTE));
    assert_eq!(result.ran, Duration::from_secs(2));
    let failure = result.failure.expect("a failure");
    assert_eq!(
        (failure.signal, failure.core),
        (Signal::ComputationError, core(1))
    );
    let outside = Script {
        signal: Some(Signal::Crash),
        at_s: 0.0,
        core: core(2),
        reset: None,
        then_crash: false,
    };
    assert!(matches!(
        rig.sim.script_trial("0002", outside),
        Err(ScriptError::Core { .. })
    ));
    let late = Script {
        signal: Some(Signal::Crash),
        at_s: f64::NAN,
        core: core(0),
        reset: None,
        then_crash: false,
    };
    assert!(matches!(
        rig.sim.script_trial("0003", late),
        Err(ScriptError::At { .. })
    ));
}

#[test]
fn dynamic_scripts_reject_replay_only_failure_signals() {
    let rig = Rig::new(&machine(2, flat_cores(2, -20, -20)), 1);
    let result = rig.sim.script_trial(
        "0001",
        Script {
            signal: Some(Signal::UncorrectedMce),
            at_s: 2.0,
            core: core(1),
            reset: None,
            then_crash: false,
        },
    );
    assert_eq!(
        result,
        Err(ScriptError::Signal {
            trial: "0001".to_owned(),
            signal: Signal::UncorrectedMce,
        })
    );
}

#[test]
fn a_crash_draws_its_reset_from_the_model() {
    let definition = with(
        2,
        &[-11, 0],
        (-10, -10),
        ModelDef {
            reset: Weights::from([(ResetKind::SyncFlood, 1.0)]),
            crash_mce: 0.0,
            ..sharp(Signal::Crash)
        },
        vec![],
    );
    let mut rig = Rig::new(&definition, 1);
    assert!(crashed(&rig.run(basic(Regime::R1, &[0], SECOND)).0));
    rig.sim.reboot();
    let boot = rig.boot();
    let reason = rig.machine.kernel.reset_reason(&boot).expect("a reason");
    assert_eq!(reason.kind, Some(ResetKind::SyncFlood));
}

#[test]
fn a_crash_reports_its_progress_and_what_ran_before_it() {
    let definition = with(2, &[-11, 0], (-10, -10), sharp(Signal::Crash), vec![]);
    let mut rig = Rig::new(&definition, 1);
    let (waited, _) = rig.run(basic(Regime::R1, &[0], MINUTE));
    let Err(partial) = waited else {
        panic!("a crash");
    };
    assert!(matches!(partial.error, Error::Crashed));
    assert!(partial.value.ran < MINUTE && partial.value.failure.is_none());
}

fn replay_of(context: BiosContext, signal: Signal) -> Replay {
    Replay::new([ReplayFact {
        context,
        regime: Regime::R7,
        workload: pick_workload(Regime::R7, 0).id.to_owned(),
        cores: cores(&[0, 1]),
        duration: MINUTE,
        profile: offsets(&[-20, -21]),
        outcome: ReplayOutcome::Failure {
            signal,
            exposure: Duration::from_secs(7),
        },
    }])
    .expect("valid facts")
}

fn replayed(signal: Signal) -> (Rig, crate::common::Waited, crate::common::Recorder) {
    replayed_by(machine(2, flat_cores(2, -50, -50)), signal)
}

fn replayed_by(
    mut definition: MachineDefinition,
    signal: Signal,
) -> (Rig, crate::common::Waited, crate::common::Recorder) {
    definition.bios = Some(vec![-20, -21]);
    let model = model(&definition);
    let replay = replay_of(model.bios_context().clone(), signal);
    let mut rig = Rig::with(
        model,
        RunSettings {
            seed: 3,
            replay: Some(replay),
            ..RunSettings::default()
        },
    );
    let work = pick_workload(Regime::R7, 0);
    let (waited, recorder) = rig.run(spec("1", Regime::R7, work, &[0, 1], MINUTE));
    (rig, waited, recorder)
}

#[test_case(Signal::Crash; "crash")]
#[test_case(Signal::UncorrectedMce; "uncorrected mce")]
fn a_replayed_crash_or_uncorrected_machine_check_happens_at_its_recorded_exposure(signal: Signal) {
    let (mut rig, waited, recorder) = replayed(signal);
    assert!(crashed(&waited));
    if signal == Signal::Crash {
        assert_eq!(
            recorder.progress,
            ["simulated replayed crash at recorded exposure"]
        );
    } else {
        assert_eq!(recorder.signals, [(core(0), Signal::UncorrectedMce)]);
    }
    assert_eq!(rig.sim.monotonic(), Duration::from_secs(7));
    rig.sim.reboot();
    let boot = rig.boot();
    let want = usize::from(signal == Signal::UncorrectedMce);
    assert_eq!(rig.mces(&boot).len(), want);
    let reason = rig.machine.kernel.reset_reason(&boot).expect("a reason");
    let kind = if signal == Signal::Crash {
        ResetKind::Watchdog
    } else {
        ResetKind::SyncFlood
    };
    assert_eq!(reason.kind, Some(kind));
}

#[test]
fn a_replayed_crash_resets_by_watchdog_whatever_the_models_reset_mix() {
    let mut definition = machine(2, flat_cores(2, -50, -50));
    definition.model.reset = Weights::from([(ResetKind::ThermalTrip, 1.0)]);
    let (mut rig, waited, _) = replayed_by(definition, Signal::Crash);
    assert!(crashed(&waited));
    assert_eq!(rig.sim.monotonic(), Duration::from_secs(7));
    rig.sim.reboot();
    let boot = rig.boot();
    let reason = rig.machine.kernel.reset_reason(&boot).expect("a reason");
    assert_eq!(reason.kind, Some(ResetKind::Watchdog));
}

#[test]
fn a_replayed_corrected_machine_check_ends_the_trial_at_its_recorded_exposure() {
    let (mut rig, waited, _) = replayed(Signal::CorrectedMce);
    let result = waited.expect("a result");
    assert_eq!(result.ran, Duration::from_secs(7));
    assert!(result.failure.is_none());
    assert_eq!(rig.sim.monotonic(), Duration::from_secs(7));
    let boot = rig.boot();
    let mces = rig.mces(&boot);
    assert_eq!(mces.len(), 1);
    assert!(mces[0].corrected);
    assert_eq!(mces[0].monotonic, Duration::from_secs(7));
}

#[test]
fn a_replayed_backend_failure_names_the_first_loaded_core_of_the_fact() {
    let (_, waited, _) = replayed(Signal::Stall);
    let result = waited.expect("a result");
    let failure = result.failure.expect("a failure");
    assert_eq!((failure.signal, failure.core), (Signal::Stall, core(0)));
    assert_eq!(result.ran, Duration::from_secs(7));
}

#[test]
fn a_trial_without_a_matching_fact_falls_back_to_the_fitted_model() {
    let mut definition = machine(2, flat_cores(2, -50, -50));
    definition.bios = Some(vec![-20, -21]);
    let model = model(&definition);
    let replay = replay_of(model.bios_context().clone(), Signal::Crash);
    let settings = RunSettings {
        seed: 3,
        replay: Some(replay),
        ..RunSettings::default()
    };
    let mut rig = Rig::with(model, settings);
    let work = pick_workload(Regime::R7, 0);
    let mut other = spec("1", Regime::R7, work, &[0, 1], MINUTE);
    other.duration = Duration::from_secs(90);
    assert!(rig.pass(other).failure.is_none(), "a different duration");
    rig.set(0, -22).expect("set");
    let matching = spec("2", Regime::R7, work, &[0, 1], MINUTE);
    assert!(rig.pass(matching).failure.is_none(), "a different profile");
    rig.sim.set_bios_context(BiosContext {
        bios_version: "changed".to_owned(),
        ..BiosContext::default()
    });
    rig.set_all(0).expect("zero");
    rig.set(0, -20).expect("set");
    rig.set(1, -21).expect("set");
    let matching = spec("3", Regime::R7, work, &[0, 1], MINUTE);
    assert!(rig.pass(matching).failure.is_none(), "another BIOS context");
}

#[test]
fn a_model_that_declares_its_context_answers_replay_under_it() {
    let mut definition = machine(2, flat_cores(2, -50, -50));
    definition.bios = Some(vec![-20, -21]);
    definition.bios_context = Some(BiosContextDef {
        bios_version: Some("DECLARED".to_owned()),
        ..BiosContextDef::default()
    });
    let model = model(&definition);
    assert!(model.bios_context_declared());
    let replay = replay_of(model.bios_context().clone(), Signal::Crash);
    let mut rig = Rig::with(
        model,
        RunSettings {
            seed: 1,
            replay: Some(replay),
            ..RunSettings::default()
        },
    );
    let work = pick_workload(Regime::R7, 0);
    assert!(crashed(
        &rig.run(spec("1", Regime::R7, work, &[0, 1], MINUTE)).0
    ));
    let _ = offset(0);
}

#[test]
fn a_regime_mix_replaces_the_pooled_mix_in_its_regime() {
    let mut definition = machine(2, flat_cores(2, -10, -10));
    definition.model = sharp(Signal::Crash);
    definition.model.regime_signals =
        [(Regime::R2, Weights::from([(Signal::ComputationError, 1.0)]))].into();
    let mut rig = Rig::new(&definition, 1);
    let result = rig.alone(0, -12, Regime::R2, 0).expect("a result");
    let failure = result.failure.expect("a failure");
    assert_eq!(
        (failure.signal, failure.core),
        (Signal::ComputationError, core(0))
    );
    assert!(
        crashed(&rig.alone(0, -12, Regime::R1, 1)),
        "R1 keeps the pooled crash"
    );
}

#[test_case(
    Source::Joint { signal: None, members: [0, 1] }, Mixes::R7Computation, &[1], Some((Signal::ComputationError, 1));
    "joint draws the regime mix"
)]
#[test_case(
    Source::Joint { signal: None, members: [0, 1] }, Mixes::None, &[1], None;
    "joint without any regime mix crashes"
)]
#[test_case(
    Source::Joint { signal: None, members: [0, 1] }, Mixes::R2Computation, &[1], Some((Signal::UnexpectedExit, 1));
    "joint in an unlisted regime draws the pooled mix"
)]
#[test_case(
    Source::Joint { signal: Some(Signal::Stall), members: [0, 1] }, Mixes::R7Computation, &[0, 1], Some((Signal::Stall, 0));
    "joint with a signal keeps it"
)]
#[test_case(
    Source::Joint { signal: None, members: [2, 3] }, Mixes::R7Computation, &[0], None;
    "joint on unloaded cores crashes"
)]
#[test_case(
    Source::Joint { signal: None, members: [2, 3] }, Mixes::R2Computation, &[0], None;
    "joint on unloaded cores in an unlisted regime crashes"
)]
#[test_case(Source::Ccd, Mixes::R7Computation, &[1], Some((Signal::ComputationError, 1)); "ccd draws the regime mix")]
#[test_case(Source::Ccd, Mixes::None, &[1], None; "ccd without a regime mix crashes")]
#[test_case(Source::Ccd, Mixes::R2Computation, &[1], Some((Signal::UnexpectedExit, 1)); "ccd in an unlisted regime draws the pooled mix")]
fn unattributed_r7_failures_draw_the_regime_mix_when_there_is_one(
    source: Source,
    mixes: Mixes,
    loaded: &[i64],
    want: Option<(Signal, i64)>,
) {
    let cores_count = if matches!(
        source,
        Source::Joint {
            members: [2, 3],
            ..
        }
    ) {
        4
    } else {
        2
    };
    let mut definition = machine(cores_count, flat_cores(cores_count, -50, -50));
    definition.model.signals = Weights::from([(Signal::UnexpectedExit, 1.0)]);
    match mixes {
        Mixes::None => {}
        Mixes::R7Computation => {
            definition.model.regime_signals =
                [(Regime::R7, Weights::from([(Signal::ComputationError, 1.0)]))].into();
        }
        Mixes::R2Computation => {
            definition.model.regime_signals =
                [(Regime::R2, Weights::from([(Signal::ComputationError, 1.0)]))].into();
        }
    }
    match source {
        Source::Joint { signal, members } => {
            let mut hot = joint(&[(members[0], -20), (members[1], -20)], 1e6);
            hot.regimes = vec![Regime::R7];
            hot.signal = signal;
            definition.joint = vec![hot];
        }
        Source::Ccd => {
            definition.ccd = Some(CcdDef {
                log_rate: 1e6f64.ln(),
                ..CcdDef::default()
            });
        }
    }
    let mut rig = Rig::new(&definition, 1);
    rig.set_all(-20).expect("set");
    let waited = rig.run(basic(Regime::R7, loaded, MINUTE)).0;
    match want {
        None => assert!(crashed(&waited), "{waited:?}"),
        Some(expected) => assert_eq!(failure_of(&waited), Some(expected)),
    }
}
