//! A warm simulated reboot keeps the crashed boot's journal and folds instead of reading them back.

use std::path::Path;
use std::time::Duration;

use togi_journal::{EventKind, MemFs, Payload};
use togi_machine::{ResetKind, TargetOffset, error_chain};
use togi_sim::{
    ArithmeticError, History, ScriptedMachine, ScriptedTopology, Simulator, TrialOutcome,
};
use togi_simrun::{Persistence, Replay, SimError, SimOutcome, events, simulate};

use crate::common::{crash_first, input, nz, passing, scripted_simulator, until_kind};

/// Crashes at every trial end, so every boot of a capped simulation crashes.
const EVERY: usize = usize::MAX;

struct Simulated {
    boots: usize,
    journal_reads: usize,
    capped: bool,
}

fn named(path: &Path, name: &str) -> bool {
    path.file_name().is_some_and(|file| file == name)
}

/// Runs a simulation whose machine crashes at each of the first `crashes` trial ends, so every boot up to then crashes.
fn simulated(replay: Replay, boots: u32, crashes: usize) -> Simulated {
    let fs = MemFs::new();
    fs.record_history();
    let persistence = Persistence::Memory(fs.clone());
    let machine = scripted_simulator(passing(ScriptedTopology::SixteenCores), 102);
    let mut sim = input(machine.clone(), persistence.clone());
    sim.observer = Some(crash_first(&machine, EventKind::TrialEnd, crashes));
    sim.replay = replay;
    sim.max_boots = nz(boots);
    let capped = matches!(simulate(sim).unwrap(), SimOutcome::Capped { .. });
    let journal_reads = fs
        .reads()
        .iter()
        .filter(|path| named(path, "events.jsonl"))
        .count();
    let boots = events(&persistence)
        .unwrap()
        .iter()
        .filter(|event| matches!(event.payload, Payload::ConfigLoaded(_)))
        .count();
    Simulated {
        boots,
        journal_reads,
        capped,
    }
}

#[test]
fn a_warm_simulation_reads_the_journal_once_however_many_boots_it_runs() {
    let short = simulated(Replay::Warm, 8, EVERY);
    let long = simulated(Replay::Warm, 24, EVERY);
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
    let short = simulated(Replay::Cold, 8, EVERY);
    let long = simulated(Replay::Cold, 24, EVERY);
    assert_eq!((short.boots, long.boots), (8, 24));
    assert!(short.capped && long.capped);
    assert!(
        long.journal_reads >= short.journal_reads + 16,
        "{} reads in 8 boots, {} in 24",
        short.journal_reads,
        long.journal_reads
    );
}

#[test]
fn an_ordinary_simulation_counts_operations_but_keeps_no_history() {
    let fs = MemFs::new();
    let mut sim = input(
        scripted_simulator(passing(ScriptedTopology::FourCores), 1),
        Persistence::Memory(fs.clone()),
    );
    sim.observer = Some(until_kind(EventKind::SessionStart));
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
fn boot_exhaustion_reaches_the_runner_and_keeps_the_journal(replay: Replay, max_boots: u32) {
    let persistence = Persistence::Memory(MemFs::new());
    let sim = Simulator::scripted(
        passing(ScriptedTopology::SixteenCores),
        1,
        History {
            boots: u32::MAX - 1,
            ..Default::default()
        },
    )
    .unwrap();
    let boot = sim.seams().host.boot_id().unwrap();
    let mut run = input(sim.clone(), persistence.clone());
    run.observer = Some(crash_first(&sim, EventKind::TrialEnd, 1));
    run.replay = replay;
    run.max_boots = nz(max_boots);
    assert!(matches!(
        simulate(run),
        Err(SimError::Reboot {
            boot: 1,
            source: ArithmeticError::BootNumber
        })
    ));
    let recorded = events(&persistence).unwrap();
    assert!(!recorded.is_empty());
    assert!(recorded.iter().all(|event| event.boot == boot));
}

#[test]
fn a_watchdog_reset_at_the_last_boot_number_reports_the_overflow_chain() {
    let script = ScriptedMachine::new(
        ScriptedTopology::FourCores,
        TargetOffset::ZERO,
        |spec, registers| {
            if spec
                .cores
                .iter()
                .any(|core| registers[core.index()] != TargetOffset::ZERO)
            {
                TrialOutcome::Reset {
                    stalled: None,
                    kind: ResetKind::Watchdog,
                    at: Duration::from_secs(1),
                }
            } else {
                TrialOutcome::Pass
            }
        },
    );
    let sim = Simulator::scripted(
        script,
        1,
        History {
            boots: u32::MAX - 1,
            ..History::default()
        },
    )
    .unwrap();
    let mut run = input(sim, Persistence::Memory(MemFs::new()));
    run.replay = Replay::Cold;
    run.max_boots = nz(1);
    let Err(err) = simulate(run) else {
        panic!("an exhausted boot number did not fail the watchdog reboot");
    };
    assert!(matches!(
        err,
        SimError::Reboot {
            boot: 1,
            source: ArithmeticError::BootNumber,
        }
    ));
    assert_eq!(
        error_chain(&err),
        "reboot the simulated machine after boot 1: allocate the next simulated boot number"
    );
}
