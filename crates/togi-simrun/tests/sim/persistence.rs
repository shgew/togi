use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use togi_journal::EventKind;
use togi_machine::BootId;
use togi_session::Stop;
use togi_sim::MachineDefinition;
use togi_simrun::{Observe, Persistence, SimInput, SimOutcome, simulate};

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
) -> (Option<Stop>, bool) {
    let mut input = input(simulator(definition, seed), persistence);
    input.max_boots = nz(BOOTS);
    match simulate(input).unwrap() {
        SimOutcome::Stopped(finished) => {
            assert!(finished.errors.is_empty(), "{:?}", finished.errors);
            (finished.stop, false)
        }
        SimOutcome::Capped { .. } => (None, true),
    }
}

fn in_memory(definition: &MachineDefinition, seed: u64) -> Left {
    let fs = togi_journal::MemFs::new();
    let (stop, capped) = run(definition, seed, Persistence::Memory(fs.clone()));
    let root = Path::new(STATE);
    Left {
        stop,
        capped,
        events: text(fs.contents(root.join("events.jsonl")).unwrap(), root),
        state: text(fs.contents(root.join("state.json")).unwrap(), root),
    }
}

fn on_disk(definition: &MachineDefinition, seed: u64) -> Left {
    let dir = tempfile::tempdir().unwrap();
    let (stop, capped) = run(
        definition,
        seed,
        Persistence::Files(dir.path().to_path_buf()),
    );
    let read = |name: &str| text(std::fs::read(dir.path().join(name)).unwrap(), dir.path());
    Left {
        stop,
        capped,
        events: read("events.jsonl"),
        state: read("state.json"),
    }
}

fn same_files(definition: &MachineDefinition, seed: u64) {
    let disk = on_disk(definition, seed);
    let memory = in_memory(definition, seed);
    assert_eq!(
        disk.stop, memory.stop,
        "stop (left on disk, right in memory)"
    );
    assert_eq!(
        disk.capped, memory.capped,
        "boot cap reached (left on disk, right in memory)"
    );
    if let Some((line, (left, right))) = disk
        .events
        .lines()
        .zip(memory.events.lines())
        .enumerate()
        .find(|(_, (left, right))| left != right)
    {
        panic!(
            "events.jsonl line {} differs\n  on disk:   {left}\n  in memory: {right}",
            line + 1
        );
    }
    assert_eq!(
        disk.events.lines().count(),
        memory.events.lines().count(),
        "events.jsonl lines (left on disk, right in memory)"
    );
    assert!(
        disk.events.lines().count() > 100,
        "a short session proves little"
    );
    assert_eq!(
        disk.state, memory.state,
        "state.json (left on disk, right in memory)"
    );
}

#[test]
fn shared_voltage_1000() {
    same_files(&machine_file("shared-voltage"), 1000);
}

#[test]
fn default_machine_1() {
    same_files(&MachineDefinition::default(), 1);
}

#[test]
fn target_fit_0_1() {
    same_files(&machine_file("target-fit-0"), 1);
}

type Seen = Rc<RefCell<Vec<(BootId, Option<Vec<u8>>)>>>;

/// Records, at every appended event, the `state.json` on disk beside the event's boot; the observer answers `stop`.
fn watch_state_json(
    input: &mut SimInput,
    dir: &Path,
    stop: impl Fn(EventKind) -> bool + 'static,
) -> Seen {
    let seen: Seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    let path: PathBuf = dir.join("state.json");
    input.observer = Some(Box::new(move |event| {
        sink.borrow_mut()
            .push((event.boot.clone(), std::fs::read(&path).ok()));
        if stop(event.kind()) {
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    seen
}

/// No run publishes `state.json` while it appends: what each boot sees is what the boot before it left.
fn assert_unpublished_during_runs(seen: &Seen) -> usize {
    let seen = seen.borrow();
    assert!(!seen.is_empty());
    assert_eq!(seen[0].1, None, "a fresh state directory had a state.json");
    let mut boots = 0;
    for run in seen.chunk_by(|left, right| left.0 == right.0) {
        boots += 1;
        assert!(
            run.iter().all(|(_, state)| *state == run[0].1),
            "state.json changed during the run of boot {:?}",
            run[0].0
        );
    }
    boots
}

#[test]
fn a_state_directory_on_disk_gets_state_json_only_when_a_run_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut input = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Files(dir.path().to_path_buf()),
    );
    let seen = watch_state_json(&mut input, dir.path(), |_| false);
    let SimOutcome::Stopped(finished) = simulate(input).unwrap() else {
        panic!("capped");
    };
    assert_eq!(finished.stop, Some(Stop::Cycles));
    let boots = assert_unpublished_during_runs(&seen);
    assert!(
        boots > 1,
        "the run never crashed, so no capsule was published"
    );
    let seen = seen.borrow();
    let mut published = seen.iter().map(|(_, state)| state).collect::<Vec<_>>();
    published.dedup();
    assert_eq!(published.len(), boots, "one publication per crashed boot");
    let journal = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
    assert!(
        !journal.contains("\"state.rebuilt\""),
        "a boot found no published state.json and rebuilt it"
    );
    assert!(dir.path().join("state.json").exists());
}

#[test]
fn a_capped_simulation_on_disk_leaves_the_crashed_runs_state_json() {
    let dir = tempfile::tempdir().unwrap();
    let mut input = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Files(dir.path().to_path_buf()),
    );
    input.max_boots = nz(1);
    let seen = watch_state_json(&mut input, dir.path(), |_| false);
    assert!(matches!(
        simulate(input).unwrap(),
        SimOutcome::Capped { .. }
    ));
    assert_eq!(assert_unpublished_during_runs(&seen), 1);
    assert!(dir.path().join("state.json").exists());
}

#[test]
fn a_signalled_run_on_disk_publishes_state_json_when_it_stops() {
    let dir = tempfile::tempdir().unwrap();
    let mut input = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Files(dir.path().to_path_buf()),
    );
    let seen = watch_state_json(&mut input, dir.path(), |kind| kind == EventKind::TrialEnd);
    let SimOutcome::Stopped(finished) = simulate(input).unwrap() else {
        panic!("capped");
    };
    assert_eq!(finished.stop, Some(Stop::Signal));
    assert_unpublished_during_runs(&seen);
    assert!(dir.path().join("state.json").exists());
}
