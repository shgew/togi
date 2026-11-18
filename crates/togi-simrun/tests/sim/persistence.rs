use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use togi_journal::{EventKind, MemFs};
use togi_session::Stop;
use togi_sim::MachineDefinition;
use togi_simrun::{Observe, Persistence, Replay, SimOutcome, simulate};

use test_case::test_case;

use crate::common::{STATE, hunt_machine, input, machine_file, nz, simulator};

const BOOTS: u32 = 20;

struct Left {
    stop: Option<Stop>,
    capped: bool,
    events: String,
}

fn text(bytes: Vec<u8>, root: &Path) -> String {
    String::from_utf8(bytes)
        .unwrap()
        .replace(root.to_str().unwrap(), STATE)
}

fn run(
    definition: &MachineDefinition,
    seed: u64,
    persistence: Persistence,
    replay: Replay,
) -> (Option<Stop>, bool) {
    let mut input = input(simulator(definition, seed), persistence);
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

fn in_memory(definition: &MachineDefinition, seed: u64, replay: Replay) -> Left {
    let fs = MemFs::new();
    let (stop, capped) = run(definition, seed, Persistence::Memory(fs.clone()), replay);
    let root = Path::new(STATE);
    Left {
        stop,
        capped,
        events: text(fs.contents(root.join("events.jsonl")).unwrap(), root),
    }
}

#[test_case(&machine_file("shared-voltage"), 1000; "shared voltage 1000")]
#[test_case(&MachineDefinition::default(), 1; "default machine 1")]
#[test_case(&machine_file("target-fit-0"), 1; "target fit 0 1")]
fn warm_and_cold_replays_leave_the_same_files(definition: &MachineDefinition, seed: u64) {
    let cold = in_memory(definition, seed, Replay::Cold);
    let warm = in_memory(definition, seed, Replay::Warm);
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
}

#[test_case(1; "seed 1")]
#[test_case(621; "seed 621")]
fn copied_journal_resumes_the_same_in_both_replay_modes(seed: u64) {
    let definition = MachineDefinition::default();
    let source = MemFs::new();
    let mut first = input(
        simulator(&definition, seed),
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
        run(
            &definition,
            seed,
            Persistence::Memory(copied.clone()),
            replay,
        );
        results.push(copied.contents(&path).unwrap());
    }
    assert_eq!(results[0], results[1]);
}

/// A boot's panic reaches the caller only after its journal is closed, so the buffered events
/// can be read back.
#[test]
fn an_observer_panic_flushes_the_boots_events_before_it_propagates() {
    let persistence = Persistence::Memory(MemFs::new());
    let mut input = input(simulator(&hunt_machine(2), 1), persistence.clone());
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
