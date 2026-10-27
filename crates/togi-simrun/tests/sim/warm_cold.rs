use togi_journal::{EventKind, MemFs, Payload};
use togi_simrun::{Persistence, Replay, SimOutcome, events, simulate};

use test_case::test_case;

use crate::common::{crash_first, input, machine_file, nz, simulator, state_json};

const BOOTS: u32 = 24;

fn run(
    machine: &str,
    seed: u64,
    replay: Replay,
) -> (Vec<togi_journal::Event>, Option<Vec<u8>>, bool) {
    let fs = MemFs::new();
    let persistence = Persistence::Memory(fs.clone());
    let machine = simulator(&machine_file(machine), seed);
    let mut sim = input(machine.clone(), persistence.clone());
    sim.observer = Some(crash_first(&machine, EventKind::TrialEnd, usize::MAX));
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

#[test]
fn a_bounded_crash_resume_has_identical_warm_and_cold_journal_and_state_bytes() {
    fn bounded(replay: Replay) -> (Vec<u8>, Vec<u8>) {
        let fs = MemFs::new();
        let persistence = Persistence::Memory(fs.clone());
        let machine = simulator(&togi_sim::MachineDefinition {
            cores: 2,
            ..togi_sim::MachineDefinition::default()
        }, 1);
        let mut sim = input(machine.clone(), persistence.clone());
        sim.stop_at = Some(machine.now() + jiff::SignedDuration::from_secs(150));
        sim.max_boots = nz(2);
        sim.replay = replay;
        sim.observer = Some(crash_first(&machine, EventKind::TrialEnd, 1));
        let outcome = simulate(sim).unwrap();
        assert!(matches!(outcome, SimOutcome::Stopped(_)), "{replay:?}");
        let journal = events(&persistence).unwrap();
        assert_eq!(journal.iter().filter(|event| matches!(event.payload, Payload::ConfigLoaded(_))).count(), 2, "{replay:?}");
        assert_eq!(journal.iter().filter(|event| matches!(event.payload, Payload::CrashDetected(_))).count(), 1, "{replay:?}");
        (fs.contents("/state/events.jsonl").unwrap(), state_json(&fs).unwrap())
    }

    assert_eq!(bounded(Replay::Warm), bounded(Replay::Cold));
}
