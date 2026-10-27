use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use togi_journal::{EventKind, MemFs};
use togi_machine::BootId;
use togi_session::Stop;
use togi_sim::MachineDefinition;
use togi_simrun::{Observe, Persistence, Replay, SimInput, SimOutcome, events, simulate};

use test_case::test_case;

use crate::common::{STATE, hunt_machine, input, machine_file, nz, simulator};

const BOOTS: u32 = 20;

struct Left {
    stop: Option<Stop>,
    capped: bool,
    events: String,
    state: String,
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
        state: text(fs.contents(root.join("state.json")).unwrap(), root),
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
    assert_eq!(cold.state, warm.state, "state.json (left cold, right warm)");
}

#[test_case(1; "seed 1")]
#[test_case(621; "seed 621")]
fn copied_snapshot_resume_matches_full_replay(seed: u64) {
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
        for name in ["events.jsonl", "state.json"] {
            let path = format!("{STATE}/{name}");
            copied.put(&path, source.contents(&path).unwrap());
        }
        run(
            &definition,
            seed,
            Persistence::Memory(copied.clone()),
            replay,
        );
        results.push((
            copied.contents(format!("{STATE}/events.jsonl")).unwrap(),
            copied.contents(format!("{STATE}/state.json")).unwrap(),
        ));
    }
    assert_eq!(results[0], results[1]);
}

struct Publications {
    boot: BootId,
    state: Option<Vec<u8>>,
    boots: usize,
    published: usize,
}

type Seen = Rc<RefCell<Option<Publications>>>;

/// Checks every event's exact `state.json` bytes online, retaining only the current boot's snapshot.
fn watch_state_json(
    input: &mut SimInput,
    fs: &MemFs,
    stop: impl Fn(EventKind) -> bool + 'static,
) -> Seen {
    let seen: Seen = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&seen);
    let fs = fs.clone();
    input.observer = Some(Box::new(move |event| {
        let state = fs.contents(format!("{STATE}/state.json"));
        let mut seen = sink.borrow_mut();
        match seen.as_mut() {
            None => {
                assert_eq!(state, None, "a fresh state directory had a state.json");
                *seen = Some(Publications {
                    boot: event.boot.clone(),
                    state,
                    boots: 1,
                    published: 1,
                });
            }
            Some(seen) => {
                if seen.boot == event.boot {
                    assert_eq!(
                        state, seen.state,
                        "state.json changed during the run of boot {:?}",
                        seen.boot
                    );
                } else {
                    seen.boots += 1;
                    seen.published += usize::from(state != seen.state);
                    seen.boot = event.boot.clone();
                    seen.state = state;
                }
            }
        }
        if stop(event.kind()) {
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    seen
}

/// Within-boot equality was checked online; return consecutive publications and boot count.
fn observed_publications(seen: &Seen) -> (usize, usize) {
    let seen = seen.borrow();
    let seen = seen.as_ref().expect("the run appended no events");
    (seen.published, seen.boots)
}

/// Runs the crashing machine to its stop in memory and returns how many distinct `state.json` files the runs saw, and
/// how many boots there were.
fn published_in_a_crashing_simulation(replay: Replay) -> (usize, usize) {
    let fs = MemFs::new();
    let mut input = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Memory(fs.clone()),
    );
    input.replay = replay;
    let seen = watch_state_json(&mut input, &fs, |_| false);
    let SimOutcome::Stopped(finished) = simulate(input).unwrap() else {
        panic!("capped");
    };
    assert_eq!(finished.stop, Some(Stop::Signal));
    let (published, boots) = observed_publications(&seen);
    assert!(
        boots > 1,
        "the run never crashed, so no capsule was published"
    );
    assert!(
        !events(&Persistence::Memory(fs.clone()))
            .unwrap()
            .iter()
            .any(|event| event.kind() == EventKind::StateRebuilt),
        "a boot found no published state.json and rebuilt it"
    );
    assert!(fs.contents(format!("{STATE}/state.json")).is_some());
    (published, boots)
}

#[test]
fn a_warm_simulation_gets_state_json_only_when_it_stops() {
    let (published, _) = published_in_a_crashing_simulation(Replay::Warm);
    assert_eq!(
        published, 1,
        "no run saw a state.json: crashes publish none"
    );
}

#[test]
fn a_cold_simulation_gets_state_json_at_every_crashed_boot() {
    let (published, boots) = published_in_a_crashing_simulation(Replay::Cold);
    assert_eq!(published, boots, "one publication per crashed boot");
}

#[test]
fn a_capped_simulation_leaves_the_crashed_runs_state_json() {
    let fs = MemFs::new();
    let mut input = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Memory(fs.clone()),
    );
    input.max_boots = nz(1);
    let seen = watch_state_json(&mut input, &fs, |_| false);
    assert!(matches!(
        simulate(input).unwrap(),
        SimOutcome::Capped { .. }
    ));
    assert_eq!(observed_publications(&seen).1, 1);
    assert!(fs.contents(format!("{STATE}/state.json")).is_some());
}

#[test]
fn a_signalled_run_publishes_state_json_when_it_stops() {
    let fs = MemFs::new();
    let mut input = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Memory(fs.clone()),
    );
    let seen = watch_state_json(&mut input, &fs, |kind| kind == EventKind::TrialEnd);
    let SimOutcome::Stopped(finished) = simulate(input).unwrap() else {
        panic!("capped");
    };
    assert_eq!(finished.stop, Some(Stop::Signal));
    observed_publications(&seen);
    assert!(fs.contents(format!("{STATE}/state.json")).is_some());
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
