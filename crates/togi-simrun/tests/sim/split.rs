use std::path::Path;

use togi_sim::{MachineDefinition, Model};
use togi_simrun::{Persistence, SimOutcome, resume_simulator, simulate};

use crate::common::{STATE, hunt_machine, input, nz, simulator};

const MACHINE: &str = "machine.json";

fn read(root: &Path, name: &str) -> String {
    String::from_utf8(std::fs::read(root.join(name)).unwrap())
        .unwrap()
        .replace(root.to_str().unwrap(), STATE)
}

fn resumed(definition: &MachineDefinition, seed: u64, root: &Path) -> togi_sim::Simulator {
    resume_simulator(
        &Persistence::Files(root.to_path_buf()),
        Model::new(definition, seed).unwrap(),
        seed,
        None,
    )
    .unwrap()
}

fn to_the_end(sim: togi_sim::Simulator, root: &Path) {
    let outcome = simulate(input(sim, Persistence::Files(root.to_path_buf()))).unwrap();
    assert!(
        matches!(outcome, SimOutcome::Stopped(_)),
        "capped at the full cap"
    );
    assert!(
        !root.join(MACHINE).exists(),
        "a stopped simulation leaves no machine"
    );
}

fn capped(definition: &MachineDefinition, seed: u64, root: &Path, boots: u32) -> serde_json::Value {
    let mut first = input(
        simulator(definition, seed),
        Persistence::Files(root.to_path_buf()),
    );
    first.max_boots = nz(boots);
    let outcome = simulate(first).unwrap();
    assert!(
        matches!(outcome, SimOutcome::Capped { .. }),
        "stopped within {boots} boots"
    );
    serde_json::from_slice(&std::fs::read(root.join(MACHINE)).unwrap()).unwrap()
}

fn split_matches_whole(definition: &MachineDefinition, seed: u64, boots: u32) -> serde_json::Value {
    let whole = tempfile::tempdir().unwrap();
    to_the_end(simulator(definition, seed), whole.path());

    let split = tempfile::tempdir().unwrap();
    let saved = capped(definition, seed, split.path(), boots);
    to_the_end(resumed(definition, seed, split.path()), split.path());

    let events = read(whole.path(), "events.jsonl");
    assert!(
        events.lines().count() > 100,
        "a short session proves little"
    );
    if let Some((line, (want, got))) = events
        .lines()
        .zip(read(split.path(), "events.jsonl").lines())
        .enumerate()
        .find(|(_, (want, got))| want != got)
    {
        panic!(
            "events.jsonl line {} differs\n  whole: {want}\n  split: {got}",
            line + 1
        );
    }
    assert_eq!(events, read(split.path(), "events.jsonl"));
    assert_eq!(
        read(whole.path(), "state.json"),
        read(split.path(), "state.json")
    );

    let forgotten = tempfile::tempdir().unwrap();
    capped(definition, seed, forgotten.path(), boots);
    std::fs::remove_file(forgotten.path().join(MACHINE)).unwrap();
    to_the_end(
        resumed(definition, seed, forgotten.path()),
        forgotten.path(),
    );
    assert_ne!(
        events,
        read(forgotten.path(), "events.jsonl"),
        "a machine rebuilt from the journals alone continued as if nothing were lost"
    );
    saved
}

#[test]
fn a_capped_simulation_continues_in_another_process_as_if_uninterrupted() {
    let saved = split_matches_whole(&hunt_machine(2), 1, 3);
    let last = saved["boots"].as_array().unwrap().last().unwrap();
    assert!(last["reason"]["kind"].is_string(), "{saved}");
}

#[test]
fn a_capped_simulation_keeps_its_pending_machine_checks_for_the_next_process() {
    let mut definition = hunt_machine(4);
    definition.model.crash_mce = 1.0;
    let saved = split_matches_whole(&definition, 1, 4);
    assert!(
        saved["boots"]
            .as_array()
            .unwrap()
            .last()
            .is_some_and(|boot| !boot["mces"].as_array().unwrap().is_empty()),
        "{saved}"
    );
}
