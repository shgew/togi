use std::time::Duration;

use test_case::test_case;
use togi_machine::{
    BankType, ControllerMode, Error, Purpose, Regime, ResetKind, Signal, TargetOffset, TrialResult,
    TrialSpec,
};
use togi_sim::{
    History, ReportedFailure, RestoreError, ScriptError, ScriptedMachine, ScriptedMce,
    ScriptedTopology, ScriptedVoltageSamples, Simulator, TrialOutcome, model::Script,
};

use crate::common::{Journal, Rig, basic, core, machine, offset};

const AFTER: Duration = Duration::from_secs(7);
const DURATION: Duration = Duration::from_secs(90);

fn rig(
    topology: ScriptedTopology,
    outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
) -> Rig {
    let inputs = ScriptedMachine::new(topology, TargetOffset::ZERO, outcome);
    rig_with(inputs)
}

fn rig_with(inputs: ScriptedMachine) -> Rig {
    let sim = Simulator::scripted(inputs, 1, History::default()).expect("a scripted machine");
    Rig {
        machine: sim.seams(),
        sim,
        journal: Journal::default(),
    }
}

fn profile_and_placement(spec: &TrialSpec, registers: &[TargetOffset]) -> TrialOutcome {
    if spec.cores == [core(0)]
        && matches!(
            spec.controller,
            ControllerMode::Managed {
                rotation: 1,
                purpose: Purpose::Exact
            }
        )
        && registers[0] == offset(-5)
        && registers[8] == offset(-10)
    {
        TrialOutcome::ComputationError {
            core: core(0),
            at: AFTER,
        }
    } else {
        TrialOutcome::Pass
    }
}

#[test_case(-5, -10, 1, true; "matching trial")]
#[test_case(-10, -10, 1, false; "different loaded register")]
#[test_case(-5, 0, 1, false; "different unloaded register")]
#[test_case(-5, -10, 0, false; "different placement")]
fn the_script_can_distinguish_profile_and_placement(
    loaded: i64,
    idle: i64,
    rotation: u64,
    fails: bool,
) {
    let mut rig = rig(ScriptedTopology::SixteenCores, profile_and_placement);
    rig.set(0, loaded).expect("set loaded core");
    rig.set(8, idle).expect("set unloaded core");
    let mut trial = basic(Regime::R2, &[0], DURATION);
    trial.controller = ControllerMode::Managed {
        rotation,
        purpose: Purpose::Exact,
    };
    let (result, report) = rig.run(trial);
    let result = result.expect("no reset");
    if fails {
        let failure = result.failure.expect("the matching trial fails");
        assert_eq!(
            (failure.core, failure.signal, result.ran),
            (core(0), Signal::ComputationError, AFTER)
        );
        assert_eq!(report.signals, [(core(0), Signal::ComputationError)]);
    } else {
        assert_eq!((result.ran, result.failure), (DURATION, None));
        assert!(report.signals.is_empty());
    }
}

fn mce(_: &TrialSpec, _: &[TargetOffset]) -> TrialOutcome {
    TrialOutcome::UncorrectedMce {
        core: core(8),
        at: AFTER,
    }
}

fn power(_: &TrialSpec, _: &[TargetOffset]) -> TrialOutcome {
    TrialOutcome::Reset {
        stalled: Some(core(0)),
        kind: ResetKind::PowerButton,
        at: AFTER,
    }
}

#[test_case(mce, ResetKind::SyncFlood; "uncorrected machine check")]
#[test_case(power, ResetKind::PowerButton; "power button reset")]
fn a_scripted_reset_and_its_evidence_survive_restoration(
    outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
    reset: ResetKind,
) {
    let mut rig = rig(ScriptedTopology::SixteenCores, outcome);
    let old_boot = rig.boot();
    rig.set(0, -5).expect("a nonstock register");
    let (result, report) = rig.run(basic(Regime::R2, &[0], DURATION));
    let failure = result.expect_err("the machine crashed");
    assert!(matches!(failure.error, togi_machine::Error::Crashed));
    assert_eq!(failure.value.ran, AFTER);
    let persisted = serde_json::to_vec(&rig.sim.persisted()).expect("encode the crashed machine");
    let resumed = Simulator::restore(
        rig.sim.source(),
        serde_json::from_slice(&persisted).expect("decode the crashed machine"),
    )
    .expect("restore it with the same source");
    resumed.reboot().expect("reboot");
    let mut machine = resumed.seams();
    let boot = machine.host.boot_id().expect("boot id");
    assert_ne!(old_boot, boot);
    assert_eq!(
        machine
            .kernel
            .reset_reason_after(&old_boot)
            .expect("reset reason")
            .kind,
        Some(reset)
    );
    assert_eq!(
        machine.smu.offset(core(0)).expect("reset register"),
        TargetOffset::ZERO.observed()
    );
    let mces = machine
        .kernel
        .mces(&boot, Duration::ZERO)
        .expect("kernel log");
    if reset == ResetKind::SyncFlood {
        assert_eq!(report.signals, [(core(8), Signal::UncorrectedMce)]);
        let [mce] = mces.as_slice() else {
            panic!("expected one MCE: {mces:?}")
        };
        assert_eq!(
            (mce.core, mce.bank, mce.bank_type, mce.corrected),
            (Some(core(8)), 0, BankType::LoadStore, false)
        );
    } else {
        assert!(report.signals.is_empty());
        assert!(mces.is_empty());
    }
}

#[test_case(true, ("fitted", "scripted"); "fitted snapshot with scripted source")]
#[test_case(false, ("scripted", "fitted"); "scripted snapshot with fitted source")]
fn restoring_with_a_different_source_kind_is_refused(
    fitted_snapshot: bool,
    expected: (&str, &str),
) {
    let fitted = Rig::new(&machine(4, vec![]), 1);
    let scripted = rig(ScriptedTopology::FourCores, |_, _| TrialOutcome::Pass);
    let (original, restoring) = if fitted_snapshot {
        (&fitted.sim, &scripted.sim)
    } else {
        (&scripted.sim, &fitted.sim)
    };
    let encoded = serde_json::to_vec(&original.persisted()).expect("encode the original machine");
    let persisted = serde_json::from_slice(&encoded).expect("decode the original machine");
    let error = Simulator::restore(restoring.source(), persisted)
        .err()
        .expect("a refusal");
    let RestoreError::SourceMismatch { persisted, given } = error else {
        panic!("expected a source mismatch: {error}");
    };
    assert_eq!((persisted, given), expected);
}

#[test_case(
    |_, _| TrialOutcome::ComputationError { core: core(4), at: AFTER },
    "simulated SMU: no core 4";
    "outcome core outside the topology"
)]
#[test_case(
    |spec, _| TrialOutcome::ComputationError {
        core: core(0),
        at: spec.duration + Duration::from_secs(1),
    },
    "scripted outcome of trial 0001 falls after its end";
    "outcome after the trial ends"
)]
fn an_invalid_scripted_outcome_does_not_complete_the_trial(
    outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
    expected: &str,
) {
    let mut rig = rig(ScriptedTopology::FourCores, outcome);
    let before = rig.sim.persisted();
    let (result, report) = rig.run(basic(Regime::R2, &[0], DURATION));
    let refusal = result.expect_err("an invalid scripted outcome");
    let Error::Other(error) = refusal.error else {
        panic!("expected an invalid outcome error: {}", refusal.error);
    };
    assert_eq!(error.to_string(), expected);
    assert_eq!(refusal.value, TrialResult::default());
    assert_eq!(report.progress.len(), 0);
    assert_eq!(report.signals.len(), 0);
    assert_eq!(rig.sim.persisted(), before);
}

#[test]
fn a_scripted_machine_refuses_fitted_trial_overrides() {
    let rig = rig(ScriptedTopology::FourCores, |_, _| TrialOutcome::Pass);
    let script = Script {
        signal: Some(Signal::ComputationError),
        at_s: 7.0,
        core: core(0),
        reset: None,
        then_crash: false,
    };
    assert_eq!(
        rig.sim.script_trial("0001", script),
        Err(ScriptError::FittedOnly)
    );
}

#[test]
fn a_scripted_machine_reports_no_preferred_core_ranking() {
    let mut rig = rig(ScriptedTopology::FourCores, |_, _| TrialOutcome::Pass);
    let missing = rig.machine.host.ranking().expect_err("no scripted ranking");
    assert_eq!(missing.value, []);
    assert_eq!(
        missing.error.to_string(),
        "simulated preferred-core ranking unavailable"
    );
}

#[test_case(ScriptedTopology::TwoCores, 2; "two cores")]
#[test_case(ScriptedTopology::EightCores, 8; "eight cores")]
fn mixed_registers_are_restored_on_each_boot(topology: ScriptedTopology, count: i64) {
    let inputs = ScriptedMachine::new(topology, TargetOffset::ZERO, |_, _| TrialOutcome::Pass)
        .with_boot_offsets(|core| offset(-5 - i64::from(core.get())));
    let mut rig = rig_with(inputs);
    assert_eq!(
        rig.machine.host.topology().unwrap().len(),
        usize::try_from(count).unwrap()
    );
    let expected: Vec<_> = (0..count)
        .map(|core| offset(-5 - core).observed())
        .collect();
    let before: Vec<_> = (0..count).map(|core| rig.read(core).unwrap()).collect();
    assert_eq!(before, expected);
    rig.set_all(-2).unwrap();
    rig.sim.reboot().unwrap();
    let after: Vec<_> = (0..count).map(|core| rig.read(core).unwrap()).collect();
    assert_eq!(after, expected);
}

#[test_case(ReportedFailure::ComputationError, Signal::ComputationError; "computation error")]
#[test_case(ReportedFailure::Stall, Signal::Stall; "stall")]
fn a_reported_backend_failure_reaches_the_reporter_before_the_reset(
    failure: ReportedFailure,
    expected: Signal,
) {
    let script: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome = match failure {
        ReportedFailure::ComputationError => |_, _| TrialOutcome::ReportedFailureThenReset {
            core: core(0),
            failure: ReportedFailure::ComputationError,
            at: AFTER,
        },
        ReportedFailure::Stall => |_, _| TrialOutcome::ReportedFailureThenReset {
            core: core(0),
            failure: ReportedFailure::Stall,
            at: AFTER,
        },
    };
    let mut rig = rig(ScriptedTopology::TwoCores, script);
    let boot = rig.boot();
    let (result, report) = rig.run(basic(Regime::R2, &[0], DURATION));
    let crash = result.unwrap_err();
    assert!(matches!(crash.error, Error::Crashed));
    assert_eq!(crash.value.ran, AFTER);
    assert_eq!(report.signals, [(core(0), expected)]);
    rig.sim.reboot().unwrap();
    assert_eq!(
        rig.machine.kernel.reset_reason_after(&boot).unwrap().kind,
        Some(ResetKind::Watchdog)
    );
}

#[test]
fn a_corrected_machine_check_logs_at_its_monotonic_time_without_resetting() {
    let mut rig = rig(ScriptedTopology::FourCores, |_, _| TrialOutcome::CorrectedMce {
        mce: ScriptedMce {
            core: core(0),
            bank: 2,
            bank_type: BankType::L2Cache,
        },
        at: AFTER,
    });
    let boot = rig.boot();
    let before = rig.sim.monotonic();
    assert_eq!(rig.pass(basic(Regime::R2, &[0], DURATION)).ran, DURATION);
    assert_eq!(rig.boot(), boot);
    let evidence = rig.mces(&boot);
    let [check] = evidence.as_slice() else {
        panic!("expected a corrected check: {evidence:?}")
    };
    assert_eq!(
        (check.core, check.bank, check.bank_type),
        (Some(core(0)), 2, BankType::L2Cache)
    );
    assert!(check.corrected);
    assert_eq!(check.monotonic, before + AFTER);
}

#[test]
fn a_named_reset_keeps_successor_boot_evidence_without_an_in_trial_signal() {
    let mut rig = rig(ScriptedTopology::SixteenCores, |_, _| TrialOutcome::ResetWithMce {
        mce: ScriptedMce {
            core: core(15),
            bank: 6,
            bank_type: BankType::L3Cache,
        },
        stalled: Some(core(0)),
        kind: ResetKind::Watchdog,
        at: AFTER,
    });
    let before = rig.boot();
    let (result, report) = rig.run(basic(Regime::R2, &[0], DURATION));
    assert!(matches!(result.unwrap_err().error, Error::Crashed));
    assert!(report.signals.is_empty());
    let sim = Simulator::restore(rig.sim.source(), rig.sim.persisted()).unwrap();
    sim.reboot().unwrap();
    let mut machine = sim.seams();
    let current = machine.host.boot_id().unwrap();
    assert_eq!(
        machine.kernel.reset_reason_after(&before).unwrap().kind,
        Some(ResetKind::Watchdog)
    );
    let evidence = machine.kernel.mces(&current, Duration::ZERO).unwrap();
    let [check] = evidence.as_slice() else {
        panic!("expected a check in the successor boot: {evidence:?}")
    };
    assert_eq!(
        (check.core, check.bank, check.bank_type, check.corrected),
        (Some(core(15)), 6, BankType::L3Cache, false)
    );
}

#[test_case(|_, _| TrialOutcome::UnexpectedExit { core: core(0), at: AFTER }, Signal::UnexpectedExit; "unexpected exit")]
#[test_case(|_, _| TrialOutcome::Stall { core: core(0), at: AFTER }, Signal::Stall; "stall")]
fn a_backend_failure_ends_the_trial_without_resetting(
    outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
    signal: Signal,
) {
    let mut rig = rig(ScriptedTopology::SixteenCores, outcome);
    let boot = rig.boot();
    let result = rig.pass(basic(Regime::R2, &[0], DURATION));
    assert_eq!(result.ran, AFTER);
    let failure = result.failure.unwrap();
    assert_eq!((failure.core, failure.signal), (core(0), signal));
    assert_eq!(rig.boot(), boot);
}

#[test]
fn voltage_samples_keep_the_written_lanes_after_snapshot_restoration() {
    static SAMPLES: ScriptedVoltageSamples = ScriptedVoltageSamples {
        requests_v: [1.25; 16],
        ccd_mhz: [5_400, 5_200],
    };
    let inputs = ScriptedMachine::sixteen_with_voltage_samples(
        TargetOffset::ZERO,
        |_, _| TrialOutcome::Pass,
        &SAMPLES,
    );
    let mut rig = rig_with(inputs);
    rig.pass(basic(Regime::R2, &[8], DURATION));
    let restored = Simulator::restore(rig.sim.source(), rig.sim.persisted()).unwrap();
    let mut machine = restored.seams();
    let sample = machine.trials.samples("0001").next().unwrap().unwrap();
    assert_eq!(sample.pm_table.unwrap().voltage_request_v, SAMPLES.requests_v);
    assert_eq!(sample.core_mhz.iter().collect::<Vec<_>>(), [(core(8), 5_200)]);
    let lanes = machine
        .host
        .preflight()
        .into_iter()
        .find(|check| check.name == "pm_table")
        .unwrap();
    assert!(lanes.ok);
    assert_eq!(
        lanes.detail,
        "scripted samples supply per-core voltage requests and loaded-core clocks"
    );
}
