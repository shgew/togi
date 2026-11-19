use std::time::Duration;

use test_case::test_case;
use togi_machine::{
    BankType, ControllerMode, Purpose, Regime, ResetKind, Signal, TargetOffset, TrialSpec,
};
use togi_sim::{History, ScriptedMachine, ScriptedTopology, Simulator, TrialOutcome};

use crate::common::{Journal, Rig, basic, core, offset};

const AFTER: Duration = Duration::from_secs(7);
const DURATION: Duration = Duration::from_secs(90);

fn rig(outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome) -> Rig {
    let inputs = ScriptedMachine::new(ScriptedTopology::SixteenCores, TargetOffset::ZERO, outcome);
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
    let mut rig = rig(profile_and_placement);
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
    let mut rig = rig(outcome);
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
