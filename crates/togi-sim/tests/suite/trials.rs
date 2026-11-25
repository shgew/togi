// Typed trial outcomes, recovery and timing through the machine seams.

use jiff::SignedDuration;
use std::time::Duration;
use togi_machine::{
    BankType, CancellationToken, Error, Regime, ResetKind, Signal, TargetOffset, TrialSpec,
};
use togi_sim::{ReportedFailure, ScriptedMce, ScriptedTopology, TrialOutcome};

use test_case::test_case;

use crate::common::{Rig, basic, core, crashed, offset};

const SECOND: Duration = Duration::from_secs(1);
const MINUTE: Duration = Duration::from_secs(60);

#[test]
fn typed_outcomes_keep_the_wall_and_monotonic_clocks_apart() {
    let rig = Rig::scripted(ScriptedTopology::TwoCores, |spec, _| {
        match spec.id.as_str() {
            "0001" => TrialOutcome::CorrectedMce {
                mce: ScriptedMce {
                    core: core(0),
                    bank: 0,
                    bank_type: BankType::LoadStore,
                },
                at: Duration::from_secs(7),
            },
            "0002" => TrialOutcome::ReportedFailureThenReset {
                core: core(1),
                failure: ReportedFailure::ComputationError,
                at: Duration::from_secs(3),
            },
            id => panic!("unexpected test trial {id}"),
        }
    });
    rig.sim.next_reset(ResetKind::ThermalTrip);
    assert_trial_and_clock_semantics(rig);
}

fn assert_trial_and_clock_semantics(mut rig: Rig) {
    let epoch = rig.sim.now();
    let boot = rig.boot();
    rig.sim
        .jump_wall(SignedDuration::from_hours(-1))
        .expect("a representable wall clock");
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
    "0002".clone_into(&mut second.id);
    let (waited, recorder) = rig.run(second);
    assert!(crashed(&waited));
    let Err(partial) = &waited else {
        panic!("a crash");
    };
    assert_eq!(partial.value.ran, Duration::from_secs(3));
    assert_eq!(rig.sim.monotonic(), Duration::from_secs(18));
    assert_eq!(recorder.signals, [(core(1), Signal::ComputationError)]);
    rig.sim.reboot().expect("a representable reboot");
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

#[test_case(
    |_, _| TrialOutcome::Reset {
        stalled: Some(core(0)),
        kind: ResetKind::Watchdog,
        at: Duration::ZERO,
    },
    &[];
    "a bare reset"
)]
#[test_case(
    |_, _| TrialOutcome::ReportedFailureThenReset {
        core: core(0),
        failure: ReportedFailure::ComputationError,
        at: Duration::ZERO,
    },
    &[(core(0), Signal::ComputationError)];
    "a backend signal"
)]
fn a_typed_reset_reports_only_the_backend_signal_if_one_was_supplied(
    outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome,
    want: &[(togi_machine::CoreId, Signal)],
) {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, outcome);
    let (waited, recorder) = rig.run(basic(Regime::R7, &[0], SECOND));
    assert!(crashed(&waited));
    assert_eq!(recorder.signals, want);
    assert!(matches!(rig.machine.host.boot_id(), Err(Error::Crashed)));
}

#[test_case(Regime::R1; "r1")]
#[test_case(Regime::R6; "r6")]
fn a_typed_reset_returns_the_exposure_and_progress_even_after_a_backend_report(regime: Regime) {
    let partial_of = |outcome: fn(&TrialSpec, &[TargetOffset]) -> TrialOutcome| {
        let mut rig = Rig::scripted(ScriptedTopology::TwoCores, outcome);
        let (waited, recorder) = rig.run(basic(regime, &[0], MINUTE));
        let Err(partial) = waited else {
            panic!("a crash");
        };
        assert!(matches!(partial.error, Error::Crashed));
        (partial.value, recorder.progress, rig.sim.monotonic())
    };
    let (ordinary, ordinary_progress, _) = partial_of(|_, _| TrialOutcome::Reset {
        stalled: Some(core(0)),
        kind: ResetKind::Watchdog,
        at: Duration::from_secs(40),
    });
    let (chained, chained_progress, monotonic) =
        partial_of(|_, _| TrialOutcome::ReportedFailureThenReset {
            core: core(0),
            failure: ReportedFailure::ComputationError,
            at: Duration::from_secs(40),
        });
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

#[test]
fn a_typed_backend_failure_reports_and_a_typed_crash_names_its_reset() {
    let rig = Rig::scripted(ScriptedTopology::TwoCores, |spec, _| {
        match spec.id.as_str() {
            "0001" => TrialOutcome::ComputationError {
                core: core(0),
                at: Duration::from_secs(2),
            },
            "0002" => TrialOutcome::Reset {
                stalled: Some(core(0)),
                kind: ResetKind::CpuShutdown,
                at: Duration::from_secs(3),
            },
            id => panic!("unexpected test trial {id}"),
        }
    });
    assert_backend_signal_and_reset(rig);
}

fn assert_backend_signal_and_reset(mut rig: Rig) {
    let (waited, recorder) = rig.run(basic(Regime::R1, &[0], MINUTE));
    let result = waited.expect("a result");
    assert_eq!(
        (result.ran, result.failure.map(|failure| failure.core)),
        (Duration::from_secs(2), Some(core(0)))
    );
    assert_eq!(recorder.signals, [(core(0), Signal::ComputationError)]);
    let mut second = basic(Regime::R1, &[0], MINUTE);
    "0002".clone_into(&mut second.id);
    assert!(crashed(&rig.run(second).0));
    rig.sim.reboot().expect("a representable reboot");
    let boot = rig.boot();
    let reason = rig.machine.kernel.reset_reason(&boot).expect("a reason");
    assert_eq!(reason.kind, Some(ResetKind::CpuShutdown));
}

#[test]
fn a_crash_reports_its_progress_and_what_ran_before_it() {
    let mut rig = Rig::scripted(ScriptedTopology::TwoCores, |_, _| TrialOutcome::Reset {
        stalled: Some(core(0)),
        kind: ResetKind::Watchdog,
        at: Duration::from_secs(7),
    });
    let (waited, _) = rig.run(basic(Regime::R1, &[0], MINUTE));
    let Err(partial) = waited else {
        panic!("a crash");
    };
    assert!(matches!(partial.error, Error::Crashed));
    assert!(partial.value.ran < MINUTE && partial.value.failure.is_none());
}

#[test_case(Regime::R8, &[8]; "r8 light core 08 beside its CCD")]
#[test_case(Regime::R9, &[0]; "r9 serial cpu on core 00")]
fn the_role_regimes_run_through_the_scripted_machine(regime: Regime, cores: &[i64]) {
    let mut rig = Rig::scripted(ScriptedTopology::SixteenCores, |spec, registers| {
        if registers == [TargetOffset::ZERO; 16] {
            TrialOutcome::Pass
        } else {
            assert_eq!(registers, [offset(-50); 16]);
            TrialOutcome::ComputationError {
                core: spec.cores[0],
                at: SECOND,
            }
        }
    });
    rig.set_all(0).expect("zero");
    let safe = rig.pass(basic(regime, cores, 120 * SECOND));
    assert!(safe.failure.is_none(), "{regime} at zero offsets");
    rig.set_all(-50).expect("deepest");
    let deep = rig.pass(basic(regime, cores, 120 * SECOND));
    assert_eq!(
        deep.failure.map(|failure| failure.signal),
        Some(Signal::ComputationError),
        "{regime} at -50"
    );
}
