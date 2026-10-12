use std::path::Path;

use togi_session::Stop;
use togi_sim::MachineDefinition;
use togi_simrun::{Persistence, SimOutcome, simulate};

use crate::common::{STATE, input, machine_file, nz, simulator};

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
