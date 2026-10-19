//! A warm simulated reboot keeps the crashed boot's journal and folds instead of reading them back, and leaves
//! `state.json` pending until the simulation ends; a cold one reads everything back and publishes at every crash.

use std::path::Path;

use togi_journal::fs::Op;
use togi_journal::{MemFs, Payload};
use togi_sim::MachineDefinition;
use togi_simrun::{Persistence, Replay, SimOutcome, events, simulate};

use crate::common::{hunt_machine, input, machine_file, nz, simulator};

struct Simulated {
    boots: usize,
    journal_reads: usize,
    state_publications: usize,
    capped: bool,
}

fn named(path: &Path, name: &str) -> bool {
    path.file_name().is_some_and(|file| file == name)
}

fn simulated(definition: &MachineDefinition, seed: u64, replay: Replay, boots: u32) -> Simulated {
    let fs = MemFs::new();
    fs.record_history();
    let persistence = Persistence::Memory(fs.clone());
    let mut sim = input(simulator(definition, seed), persistence.clone());
    sim.replay = replay;
    sim.max_boots = nz(boots);
    let capped = matches!(simulate(sim).unwrap(), SimOutcome::Capped { .. });
    let journal_reads = fs
        .reads()
        .iter()
        .filter(|path| named(path, "events.jsonl"))
        .count();
    let state_publications = fs
        .log()
        .iter()
        .filter(|(op, path)| *op == Op::Rename && named(path, ".state.json.tmp"))
        .count();
    let boots = events(&persistence)
        .unwrap()
        .iter()
        .filter(|event| matches!(event.payload, Payload::ConfigLoaded(_)))
        .count();
    Simulated {
        boots,
        journal_reads,
        state_publications,
        capped,
    }
}

#[test]
fn a_warm_simulation_reads_the_journal_once_however_many_boots_it_runs() {
    let machine = machine_file("flat-hazard");
    let short = simulated(&machine, 102, Replay::Warm, 8);
    let long = simulated(&machine, 102, Replay::Warm, 24);
    assert_eq!((short.boots, long.boots), (8, 24));
    assert!(short.capped && long.capped);
    assert!(short.journal_reads > 0, "the first boot reads the journal");
    assert_eq!(
        short.journal_reads, long.journal_reads,
        "16 more boots read the journal again"
    );
}

#[test]
fn a_cold_simulation_reads_the_journal_at_every_boot() {
    let machine = machine_file("flat-hazard");
    let short = simulated(&machine, 102, Replay::Cold, 8);
    let long = simulated(&machine, 102, Replay::Cold, 24);
    assert!(
        long.journal_reads >= short.journal_reads + 16,
        "{} reads in 8 boots, {} in 24",
        short.journal_reads,
        long.journal_reads
    );
}

#[test]
fn a_warm_simulation_publishes_state_json_once_at_its_stop_or_its_cap() {
    let stopped = simulated(&hunt_machine(2), 1, Replay::Warm, 1000);
    assert!(stopped.boots > 1 && !stopped.capped);
    assert_eq!(stopped.state_publications, 1);

    let capped = simulated(&machine_file("flat-hazard"), 102, Replay::Warm, 24);
    assert!(capped.capped);
    assert_eq!(capped.state_publications, 1);
}

#[test]
fn a_cold_simulation_publishes_state_json_at_every_crash_and_at_its_stop() {
    let stopped = simulated(&hunt_machine(2), 1, Replay::Cold, 1000);
    assert!(stopped.boots > 1 && !stopped.capped);
    assert_eq!(stopped.state_publications, stopped.boots);

    let capped = simulated(&machine_file("flat-hazard"), 102, Replay::Cold, 24);
    assert!(capped.capped);
    assert_eq!(capped.state_publications, capped.boots);
}

#[test]
fn an_ordinary_simulation_counts_operations_but_keeps_no_history() {
    let fs = MemFs::new();
    let mut sim = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Memory(fs.clone()),
    );
    sim.max_boots = nz(3);
    simulate(sim).unwrap();
    assert!(fs.mutations() > 0);
    assert!(
        std::panic::catch_unwind(|| fs.log()).is_err(),
        "the simulation recorded a history"
    );
}

#[test_case::test_case(Replay::Warm, 2; "warm reboot")]
#[test_case::test_case(Replay::Cold, 2; "cold reboot")]
#[test_case::test_case(Replay::Warm, 1; "boot cap")]
fn boot_exhaustion_reaches_the_runner_and_publishes_pending_state(replay: Replay, max_boots: u32) {
    let fs = MemFs::new();
    let persistence = Persistence::Memory(fs.clone());
    let definition = MachineDefinition::default();
    let sim = togi_sim::Simulator::new(
        togi_sim::Model::new(&definition, 1).unwrap(),
        togi_sim::RunSettings {
            seed: 1,
            history: togi_sim::History {
                boots: u32::MAX - 1,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let boot = sim.seams().host.boot_id().unwrap();
    sim.crash_before_apply(1);
    let mut run = input(sim, persistence.clone());
    run.replay = replay;
    run.max_boots = nz(max_boots);
    assert!(matches!(
        simulate(run),
        Err(togi_simrun::SimError::Reboot {
            boot: 1,
            source: togi_sim::ArithmeticError::BootNumber
        })
    ));
    assert!(
        crate::common::state_json(&fs).is_some(),
        "the crashed boot's pending state is published"
    );
    let recorded = events(&persistence).unwrap();
    assert!(!recorded.is_empty());
    assert!(recorded.iter().all(|event| event.boot == boot));
}
