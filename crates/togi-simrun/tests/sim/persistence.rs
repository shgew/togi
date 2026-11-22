use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use togi_journal::{Event, EventKind, MemFs, Payload};
use togi_machine::{BankType, ControllerMode, ResetKind, Signal, TargetOffset, TrialSpec};
use togi_session::Stop;
use togi_sim::{ScriptedMachine, ScriptedMce, ScriptedTopology, TrialOutcome};
use togi_simrun::{Observe, Persistence, Replay, SimOutcome, events, simulate};

use test_case::test_case;

use crate::common::{STATE, core, input, nz, passing, scripted_simulator, voltage_samples};

const BOOTS: u32 = 20;

struct Left {
    stop: Option<Stop>,
    capped: bool,
    events: String,
    records: Vec<Event>,
}

fn text(bytes: Vec<u8>, root: &Path) -> String {
    String::from_utf8(bytes)
        .unwrap()
        .replace(root.to_str().unwrap(), STATE)
}

fn run(
    script: ScriptedMachine,
    seed: u64,
    persistence: Persistence,
    replay: Replay,
) -> (Option<Stop>, bool) {
    let mut input = input(scripted_simulator(script, seed), persistence);
    input.replay = replay;
    input.max_boots = nz(BOOTS);
    match simulate(input).unwrap() {
        SimOutcome::Stopped(finished) => {
            assert!(finished.errors.is_empty(), "{:?}", finished.errors);
            (finished.stop, false)
        }
        SimOutcome::Capped { .. } => (None, true),
    }
}

fn in_memory(script: ScriptedMachine, seed: u64, replay: Replay) -> Left {
    let fs = MemFs::new();
    let persistence = Persistence::Memory(fs.clone());
    let (stop, capped) = run(script, seed, persistence.clone(), replay);
    let root = Path::new(STATE);
    Left {
        stop,
        capped,
        events: text(fs.contents(root.join("events.jsonl")).unwrap(), root),
        records: events(&persistence).unwrap(),
    }
}

#[derive(Clone, Copy)]
enum Stratum {
    SharedVoltage,
    BackendAndKernelFailures,
    NoReboot,
}

fn backend_and_kernel_failures(spec: &TrialSpec, _: &[TargetOffset]) -> TrialOutcome {
    let ControllerMode::Managed { rotation, .. } = spec.controller;
    match rotation {
        134 => TrialOutcome::UnexpectedExit {
            core: core(5),
            at: Duration::from_millis(11_927),
        },
        394 => TrialOutcome::Reset {
            stalled: None,
            kind: ResetKind::Watchdog,
            at: Duration::from_millis(1_386),
        },
        587 => TrialOutcome::CorrectedMce {
            mce: ScriptedMce {
                core: core(8),
                bank: 2,
                bank_type: BankType::L2Cache,
            },
            at: Duration::from_millis(24_564),
        },
        673 => TrialOutcome::ComputationError {
            core: core(5),
            at: Duration::from_millis(85_751),
        },
        _ => TrialOutcome::Pass,
    }
}

fn computation_error_without_reboot(spec: &TrialSpec, _: &[TargetOffset]) -> TrialOutcome {
    let ControllerMode::Managed { rotation, .. } = spec.controller;
    if rotation == 796 {
        TrialOutcome::ComputationError {
            core: core(3),
            at: Duration::from_millis(8_796),
        }
    } else {
        TrialOutcome::Pass
    }
}

fn unattributed_voltage_reset(spec: &TrialSpec, _: &[TargetOffset]) -> TrialOutcome {
    let ControllerMode::Managed { rotation, .. } = spec.controller;
    if rotation == 757 {
        TrialOutcome::Reset {
            stalled: None,
            kind: ResetKind::Watchdog,
            at: Duration::from_millis(560),
        }
    } else {
        TrialOutcome::Pass
    }
}

#[test_case(Stratum::SharedVoltage, 1000; "shared voltage 1000")]
#[test_case(Stratum::BackendAndKernelFailures, 1; "default machine 1")]
#[test_case(Stratum::NoReboot, 1; "target fit 0 1")]
fn warm_and_cold_replays_leave_the_same_files(stratum: Stratum, seed: u64) {
    let script = match stratum {
        Stratum::SharedVoltage => voltage_samples(unattributed_voltage_reset),
        Stratum::BackendAndKernelFailures => ScriptedMachine::new(
            ScriptedTopology::SixteenCores,
            TargetOffset::ZERO,
            backend_and_kernel_failures,
        ),
        Stratum::NoReboot => ScriptedMachine::new(
            ScriptedTopology::SixteenCores,
            TargetOffset::ZERO,
            computation_error_without_reboot,
        ),
    };
    let cold = in_memory(script, seed, Replay::Cold);
    let warm = in_memory(script, seed, Replay::Warm);
    assert_eq!(cold.stop, warm.stop, "stop (left cold, right warm)");
    assert_eq!(
        cold.capped, warm.capped,
        "boot cap reached (left cold, right warm)"
    );
    if let Some((line, (left, right))) = cold
        .events
        .lines()
        .zip(warm.events.lines())
        .enumerate()
        .find(|(_, (left, right))| left != right)
    {
        panic!(
            "events.jsonl line {} differs\n  cold: {left}\n  warm: {right}",
            line + 1
        );
    }
    assert_eq!(
        cold.events.lines().count(),
        warm.events.lines().count(),
        "events.jsonl lines (left cold, right warm)"
    );
    assert!(
        cold.events.lines().count() > 100,
        "a short session proves little"
    );
    assert_eq!(cold.stop, Some(Stop::Signal));
    assert!(!cold.capped);
    assert_stratum(stratum, &cold.records);
}

fn assert_stratum(stratum: Stratum, records: &[Event]) {
    let mut boots = 0;
    let mut crashes = 0;
    let mut corrected = 0;
    let mut voltage_ends = 0;
    let mut failures = Vec::new();
    for event in records {
        if let Payload::ConfigLoaded(_) = &event.payload {
            boots += 1;
        } else if let Payload::CrashDetected(crash) = &event.payload {
            assert_eq!(crash.reset_reason, Some(ResetKind::Watchdog));
            assert!(crash.in_flight.is_some());
            crashes += 1;
        } else if let Payload::Mce(mce) = &event.payload
            && mce.corrected
        {
            assert_eq!(mce.core, Some(core(8)));
            corrected += 1;
        } else if let Payload::TrialEnd(end) = &event.payload {
            if let Some(signal) = end.signal {
                failures.push((signal, end.core));
            }
            if end.voltage_request_median_v.is_some() && !end.voltage_requests_v.is_empty() {
                voltage_ends += 1;
            }
        }
    }
    match stratum {
        Stratum::SharedVoltage => {
            assert_eq!((boots, crashes, corrected), (2, 1, 0));
            assert_eq!(failures, [(Signal::Crash, None)]);
            assert!(voltage_ends > 800, "{voltage_ends} voltage summaries");
        }
        Stratum::BackendAndKernelFailures => {
            assert_eq!((boots, crashes, corrected), (2, 1, 1));
            assert_eq!(
                failures,
                [
                    (Signal::UnexpectedExit, Some(core(5))),
                    (Signal::Crash, None),
                    (Signal::CorrectedMce, None),
                    (Signal::ComputationError, Some(core(5))),
                ]
            );
        }
        Stratum::NoReboot => {
            assert_eq!((boots, crashes, corrected), (1, 0, 0));
            assert_eq!(failures, [(Signal::ComputationError, Some(core(3)))]);
        }
    }
}

#[test_case(1; "seed 1")]
#[test_case(621; "seed 621")]
fn copied_journal_resumes_the_same_in_both_replay_modes(seed: u64) {
    let script = passing(ScriptedTopology::SixteenCores);
    let source = MemFs::new();
    let mut first = input(
        scripted_simulator(script, seed),
        Persistence::Memory(source.clone()),
    );
    first.observer = Some(Box::new(|event| {
        if event.kind() == EventKind::TrialEnd {
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    simulate(first).unwrap();
    let mut results = Vec::new();
    for replay in [Replay::Warm, Replay::Cold] {
        let copied = MemFs::new();
        let path = format!("{STATE}/events.jsonl");
        copied.put(&path, source.contents(&path).unwrap());
        run(script, seed, Persistence::Memory(copied.clone()), replay);
        results.push(copied.contents(&path).unwrap());
    }
    assert_eq!(results[0], results[1]);
}

/// A boot's panic reaches the caller only after its journal is closed, so the buffered events
/// can be read back.
#[test]
fn an_observer_panic_flushes_the_boots_events_before_it_propagates() {
    let persistence = Persistence::Memory(MemFs::new());
    let mut input = input(
        scripted_simulator(passing(ScriptedTopology::FourCores), 1),
        persistence.clone(),
    );
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    input.observer = Some(Box::new(move |event| {
        sink.borrow_mut().push(event.seq);
        assert!(event.kind() != EventKind::TrialEnd, "a bug in the observer");
        Observe::Continue
    }));
    let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(simulate(input))))
        .expect_err("the observer's panic propagates");
    assert_eq!(
        payload.downcast_ref::<&str>().copied(),
        Some("a bug in the observer")
    );
    let recorded: Vec<_> = togi_simrun::events(&persistence)
        .unwrap()
        .iter()
        .map(|event| event.seq)
        .collect();
    assert_eq!(
        recorded,
        *seen.borrow(),
        "every event the boot appended is persisted"
    );
}
