use togi_journal::{EventKind, MemFs, Payload};
use togi_machine::{BankType, ControllerMode, Regime, ResetKind, TargetOffset, TrialSpec};
use togi_sim::{ScriptedMachine, ScriptedMce, ScriptedTopology, TrialOutcome};
use togi_simrun::{Persistence, Replay, SimOutcome, events, simulate};

use test_case::test_case;

use crate::common::{core, crash_first, input, nz, passing, scripted_simulator, voltage_samples};

const BOOTS: u32 = 24;

fn run(script: ScriptedMachine, seed: u64, replay: Replay) -> (Vec<togi_journal::Event>, bool) {
    let persistence = Persistence::Memory(MemFs::new());
    let machine = scripted_simulator(script, seed);
    let mut sim = input(machine.clone(), persistence.clone());
    sim.observer = Some(crash_first(&machine, EventKind::TrialEnd, usize::MAX));
    sim.replay = replay;
    sim.max_boots = nz(BOOTS);
    let capped = matches!(simulate(sim).unwrap(), SimOutcome::Capped { .. });
    (events(&persistence).unwrap(), capped)
}

#[derive(Clone, Copy, Debug)]
enum Stratum {
    BetweenTrials,
    UnloadedMachineCheck,
    SharedVoltage,
}

fn unloaded_machine_check(spec: &TrialSpec, _: &[TargetOffset]) -> TrialOutcome {
    let ControllerMode::Managed { rotation, .. } = spec.controller;
    if rotation == 1 {
        TrialOutcome::ResetWithMce {
            mce: ScriptedMce {
                core: core(15),
                bank: 2,
                bank_type: BankType::L2Cache,
            },
            stalled: None,
            kind: ResetKind::Watchdog,
            at: std::time::Duration::from_secs(1),
        }
    } else {
        TrialOutcome::Pass
    }
}

#[test_case(Stratum::BetweenTrials, 102; "flat hazard 102")]
#[test_case(Stratum::BetweenTrials, 7; "flat hazard 7")]
#[test_case(Stratum::UnloadedMachineCheck, 3; "misleading mce 3")]
#[test_case(Stratum::SharedVoltage, 1000; "shared voltage 1000")]
fn warm_equals_cold(stratum: Stratum, seed: u64) {
    let script = match stratum {
        Stratum::BetweenTrials => passing(ScriptedTopology::SixteenCores),
        Stratum::UnloadedMachineCheck => ScriptedMachine::new(
            ScriptedTopology::SixteenCores,
            TargetOffset::ZERO,
            unloaded_machine_check,
        ),
        Stratum::SharedVoltage => voltage_samples(|_, _| TrialOutcome::Pass),
    };
    let (warm, warm_capped) = run(script, seed, Replay::Warm);
    let (cold, cold_capped) = run(script, seed, Replay::Cold);
    let boots = warm
        .iter()
        .filter(|event| matches!(event.payload, Payload::ConfigLoaded(_)))
        .count();
    assert!(boots >= 20, "{stratum:?} {seed}: {boots} boots");
    assert_eq!(boots, BOOTS as usize);
    assert!(warm_capped);
    assert_eq!(warm_capped, cold_capped);
    assert_eq!(warm, cold);

    let ends: Vec<_> = warm
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
        })
        .collect();
    let crashes: Vec<_> = warm
        .iter()
        .filter_map(|event| {
            if let Payload::CrashDetected(crash) = &event.payload {
                Some(crash)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(crashes.len(), BOOTS as usize - 1);
    assert!(
        crashes
            .iter()
            .all(|crash| crash.reset_reason == Some(ResetKind::Watchdog))
    );
    match stratum {
        Stratum::BetweenTrials | Stratum::SharedVoltage => {
            assert_eq!(ends.len(), BOOTS as usize);
            assert!(ends.iter().all(|end| end.signal.is_none()));
            assert!(crashes.iter().all(|crash| crash.in_flight.is_none()));
            assert!(
                warm.iter()
                    .all(|event| !matches!(event.payload, Payload::Mce(_)))
            );
        }
        Stratum::UnloadedMachineCheck => {
            assert_eq!(ends.len(), BOOTS as usize - 1);
            let Some(in_flight) = crashes[0].in_flight else {
                panic!("the first watchdog reset interrupts no trial");
            };
            let Some(Payload::TrialIntent(intent)) = warm
                .iter()
                .find(|event| event.seq == in_flight)
                .map(|event| &event.payload)
            else {
                panic!("the in-flight intent is not a trial");
            };
            assert_eq!(intent.regime, Regime::R1);
            assert!(!intent.cores.contains(core(15)));
            assert!(
                warm.iter()
                    .any(|event| matches!(&event.payload, Payload::Mce(mce)
                if mce.core == Some(core(15)) && !mce.corrected))
            );
            assert_eq!(ends[0].trial, intent.trial);
            assert_ne!(ends[0].core, Some(core(15)), "an unloaded core was blamed");
            assert!(ends[1..].iter().all(|end| end.signal.is_none()));
            assert!(crashes[1..].iter().all(|crash| crash.in_flight.is_none()));
        }
    }
    if let Stratum::SharedVoltage = stratum {
        assert!(ends.iter().all(|end| end.voltage_request_median_v.is_some()
            && end.voltage_request_min_v.is_some()
            && !end.voltage_requests_v.is_empty()
            && !end.top_requesters.is_empty()
            && !end.ccd_mhz.is_empty()));
    }
}

#[test]
fn a_bounded_crash_resume_has_identical_warm_and_cold_journal_bytes() {
    fn bounded(replay: Replay) -> Vec<u8> {
        let fs = MemFs::new();
        let persistence = Persistence::Memory(fs.clone());
        let machine = scripted_simulator(passing(ScriptedTopology::TwoCores), 1);
        let mut sim = input(machine.clone(), persistence.clone());
        sim.stop_at = Some(machine.now() + jiff::SignedDuration::from_secs(150));
        sim.max_boots = nz(2);
        sim.replay = replay;
        sim.observer = Some(crash_first(&machine, EventKind::TrialEnd, 1));
        let SimOutcome::Stopped(finished) = simulate(sim).unwrap() else {
            panic!("{replay:?} did not stop");
        };
        let journal = events(&persistence).unwrap();
        finished.capsule.unwrap().verify(&journal).unwrap();
        assert_eq!(
            journal
                .iter()
                .filter(|event| matches!(event.payload, Payload::ConfigLoaded(_)))
                .count(),
            2,
            "{replay:?}"
        );
        assert_eq!(
            journal
                .iter()
                .filter(|event| matches!(event.payload, Payload::CrashDetected(_)))
                .count(),
            1,
            "{replay:?}"
        );
        fs.contents("/state/events.jsonl").unwrap()
    }

    assert_eq!(bounded(Replay::Warm), bounded(Replay::Cold));
}
