use togi_journal::{MemFs, Payload};
use togi_simrun::{Persistence, Replay, SimOutcome, events, simulate};

use test_case::test_case;

use crate::common::{input, machine_file, nz, simulator, state_json};

const BOOTS: u32 = 24;

fn run(
    machine: &str,
    seed: u64,
    replay: Replay,
) -> (Vec<togi_journal::Event>, Option<Vec<u8>>, bool) {
    let fs = MemFs::new();
    let persistence = Persistence::Memory(fs.clone());
    let mut sim = input(simulator(&machine_file(machine), seed), persistence.clone());
    sim.replay = replay;
    sim.max_boots = nz(BOOTS);
    let capped = matches!(simulate(sim).unwrap(), SimOutcome::Capped { .. });
    (events(&persistence).unwrap(), state_json(&fs), capped)
}

#[test_case("flat-hazard", 102; "flat hazard 102")]
#[test_case("flat-hazard", 7; "flat hazard 7")]
#[test_case("misleading-mce", 3; "misleading mce 3")]
#[test_case("shared-voltage", 1000; "shared voltage 1000")]
fn warm_equals_cold(machine: &str, seed: u64) {
    let (warm, warm_state, warm_capped) = run(machine, seed, Replay::Warm);
    let (cold, cold_state, cold_capped) = run(machine, seed, Replay::Cold);
    let boots = warm
        .iter()
        .filter(|event| matches!(event.payload, Payload::ConfigLoaded(_)))
        .count();
    assert!(boots >= 20, "{machine} {seed}: {boots} boots");
    assert_eq!(warm_capped, cold_capped);
    assert_eq!(warm, cold);
    assert_eq!(warm_state, cold_state);
}
