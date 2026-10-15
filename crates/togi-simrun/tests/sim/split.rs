use std::path::{Path, PathBuf};

use togi_config::Config;
use togi_journal::MemFs;
use togi_journal::fs::{Fault, Fs, Op};
use togi_session::SessionError;
use togi_sim::{MachineDefinition, Model};
use togi_simrun::{Persistence, ResumeError, SimError, SimOutcome, resume_simulator, simulate};

use crate::common::{STATE, hunt_machine, input, nz, simulator};

const MACHINE: &str = "machine.json";

/// A state directory a simulation can be split across processes in.
trait Store {
    fn persistence(&self) -> Persistence;
    /// Where the simulation keeps `name`.
    fn path(&self, name: &str) -> PathBuf;
    fn bytes(&self, name: &str) -> Option<Vec<u8>>;
    fn remove(&self, name: &str);

    /// `name` as text, with the state directory written as [`STATE`] so two directories compare.
    fn text(&self, name: &str) -> String {
        String::from_utf8(self.bytes(name).unwrap()).unwrap()
    }
}

impl Store for MemFs {
    fn persistence(&self) -> Persistence {
        Persistence::Memory(self.clone())
    }

    fn path(&self, name: &str) -> PathBuf {
        Path::new(STATE).join(name)
    }

    fn bytes(&self, name: &str) -> Option<Vec<u8>> {
        self.contents(self.path(name))
    }

    fn remove(&self, name: &str) {
        Fs::remove_file(self, &self.path(name)).unwrap();
    }
}

fn resumed(definition: &MachineDefinition, seed: u64, store: &impl Store) -> togi_sim::Simulator {
    resume_simulator(
        &store.persistence(),
        Model::new(definition, seed).unwrap(),
        seed,
        None,
    )
    .unwrap()
}

fn to_the_end(sim: togi_sim::Simulator, store: &impl Store) {
    let outcome = simulate(input(sim, store.persistence())).unwrap();
    assert!(
        matches!(outcome, SimOutcome::Stopped(_)),
        "capped at the full cap"
    );
    assert!(
        store.bytes(MACHINE).is_none(),
        "a stopped simulation leaves no machine"
    );
}

fn capped(
    definition: &MachineDefinition,
    seed: u64,
    store: &impl Store,
    boots: u32,
) -> serde_json::Value {
    let mut first = input(simulator(definition, seed), store.persistence());
    first.max_boots = nz(boots);
    let outcome = simulate(first).unwrap();
    assert!(
        matches!(outcome, SimOutcome::Capped { .. }),
        "stopped within {boots} boots"
    );
    serde_json::from_slice(&store.bytes(MACHINE).unwrap()).unwrap()
}

fn split_matches_whole<S: Store>(
    new: impl Fn() -> S,
    definition: &MachineDefinition,
    seed: u64,
    boots: u32,
) -> serde_json::Value {
    let whole = new();
    to_the_end(simulator(definition, seed), &whole);

    let split = new();
    let saved = capped(definition, seed, &split, boots);
    to_the_end(resumed(definition, seed, &split), &split);

    let events = whole.text("events.jsonl");
    assert!(
        events.lines().count() > 100,
        "a short session proves little"
    );
    if let Some((line, (want, got))) = events
        .lines()
        .zip(split.text("events.jsonl").lines())
        .enumerate()
        .find(|(_, (want, got))| want != got)
    {
        panic!(
            "events.jsonl line {} differs\n  whole: {want}\n  split: {got}",
            line + 1
        );
    }
    assert_eq!(events, split.text("events.jsonl"));
    assert_eq!(whole.text("state.json"), split.text("state.json"));

    let forgotten = new();
    capped(definition, seed, &forgotten, boots);
    forgotten.remove(MACHINE);
    to_the_end(resumed(definition, seed, &forgotten), &forgotten);
    assert_ne!(
        events,
        forgotten.text("events.jsonl"),
        "a machine rebuilt from the journals alone continued as if nothing were lost"
    );
    saved
}

fn mce_machine() -> MachineDefinition {
    let mut definition = hunt_machine(4);
    definition.model.crash_mce = 1.0;
    definition
}

fn keeps_pending_machine_checks(saved: &serde_json::Value) {
    assert!(
        saved["boots"]
            .as_array()
            .unwrap()
            .last()
            .is_some_and(|boot| !boot["mces"].as_array().unwrap().is_empty()),
        "{saved}"
    );
}

#[test]
fn a_capped_simulation_continues_in_another_process_as_if_uninterrupted() {
    let saved = split_matches_whole(MemFs::new, &hunt_machine(2), 1, 3);
    let last = saved["boots"].as_array().unwrap().last().unwrap();
    assert!(last["reason"]["kind"].is_string(), "{saved}");
}

#[test]
fn a_capped_simulation_keeps_its_pending_machine_checks_for_the_next_process() {
    keeps_pending_machine_checks(&split_matches_whole(MemFs::new, &mce_machine(), 1, 4));
}

#[test]
fn a_continuation_that_starts_nothing_keeps_the_saved_machine() {
    let definition = hunt_machine(2);
    let whole = MemFs::new();
    to_the_end(simulator(&definition, 1), &whole);

    let split = MemFs::new();
    let saved = capped(&definition, 1, &split, 3);
    let mut rejected = input(resumed(&definition, 1, &split), split.persistence());
    rejected.config = Config::parse(r#"{"start_offsets": {"15": -10}}"#).unwrap();
    let SimOutcome::Stopped(finished) = simulate(rejected).unwrap() else {
        panic!("a refused configuration capped the simulation");
    };
    assert!(
        matches!(
            finished.errors.as_slice(),
            [SessionError::NoSuchCore { .. }]
        ),
        "{:?}",
        finished.errors
    );
    let kept: serde_json::Value = serde_json::from_slice(&split.bytes(MACHINE).unwrap()).unwrap();
    assert_eq!(kept, saved);

    to_the_end(resumed(&definition, 1, &split), &split);
    assert_eq!(whole.text("events.jsonl"), split.text("events.jsonl"));
}

fn unreadable_machine(store: &impl Store) {
    let Err(ResumeError::Read { path, .. }) = resume_simulator(
        &store.persistence(),
        Model::new(&hunt_machine(2), 1).unwrap(),
        1,
        None,
    ) else {
        panic!("an unreadable machine was not a read failure");
    };
    assert_eq!(path, store.path(MACHINE));
}

#[test]
fn a_machine_that_cannot_be_read_stops_the_continuation() {
    let store = MemFs::new();
    capped(&hunt_machine(2), 1, &store, 3);
    let saved = store.bytes(MACHINE).unwrap();
    store.inject(Fault::before(Op::Read, 1).named(MACHINE));
    unreadable_machine(&store);
    assert_eq!(store.bytes(MACHINE), Some(saved));
}

#[test]
fn a_machine_path_that_is_a_directory_stops_the_continuation() {
    let store = MemFs::new();
    Fs::create_dir(&store, Path::new(STATE)).unwrap();
    Fs::create_dir(&store, &store.path(MACHINE)).unwrap();
    unreadable_machine(&store);
    assert!(store.bytes(MACHINE).is_none());
}

#[test]
fn a_continuation_that_cannot_remove_the_superseded_machine_fails() {
    let definition = hunt_machine(2);
    let store = MemFs::new();
    capped(&definition, 1, &store, 3);
    let saved = store.bytes(MACHINE).unwrap();
    let journaled = store.text("events.jsonl").lines().count();
    let sim = resumed(&definition, 1, &store);
    store.inject(Fault::before(Op::Remove, 1).named(MACHINE));

    let Err(SimError::Machine { path, .. }) = simulate(input(sim, store.persistence())) else {
        panic!("a kept superseded machine was not a machine failure");
    };
    assert_eq!(path, store.path(MACHINE));
    assert!(
        store.text("events.jsonl").lines().count() > journaled,
        "the continuation appended before it removed the machine"
    );
    assert_eq!(store.bytes(MACHINE), Some(saved));
}

#[test]
fn a_continuation_that_fails_after_a_warm_crash_still_publishes_the_crashed_boots_state() {
    let definition = hunt_machine(2);
    let store = MemFs::new();
    capped(&definition, 1, &store, 3);
    let before = store.bytes("state.json");
    let reference = MemFs::new();
    capped(&definition, 1, &reference, 3);
    let mut one_boot = input(resumed(&definition, 1, &reference), reference.persistence());
    one_boot.max_boots = nz(1);
    assert!(matches!(
        simulate(one_boot).unwrap(),
        SimOutcome::Capped { .. }
    ));

    let sim = resumed(&definition, 1, &store);
    store.inject(Fault::before(Op::Remove, 1).named(MACHINE));
    assert!(matches!(
        simulate(input(sim, store.persistence())),
        Err(SimError::Machine { .. })
    ));

    assert_ne!(store.bytes("state.json"), before);
    assert_eq!(store.bytes("state.json"), reference.bytes("state.json"));
}
