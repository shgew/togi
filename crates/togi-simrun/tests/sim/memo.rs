use togi_journal::{Event, Payload};
use togi_simrun::simulate;
use togi_tuner::{State, replay};

use test_case::test_case;

use crate::common::{Bench, machine_file, nz};

const CHECKPOINT: usize = 64;

fn asked(payload: &Payload) -> bool {
    matches!(
        payload,
        Payload::TrialIntent(_)
            | Payload::HostRanking(_)
            | Payload::Failure(_)
            | Payload::TunerWarning(_)
            | Payload::DeadEnd(_)
            | Payload::CorePhase(_)
            | Payload::TunerDecision(_)
            | Payload::ProfileChange(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
    )
}

fn simulated(machine: &str, seed: u64, boots: u32) -> Vec<Event> {
    let bench = Bench::new(&machine_file(machine), seed);
    let mut input = bench.input();
    input.max_boots = nz(boots);
    simulate(input).unwrap();
    bench.events()
}

fn fresh(prefix: &[Event]) -> State {
    replay(prefix).unwrap()
}

#[test_case("flat-hazard", 7, 30; "flat hazard 7")]
#[test_case("target-shared-voltage", 2, 40; "target shared voltage 2")]
#[test_case("target-r7-request-gap", 1, 40; "target r7 request gap 1")]
#[test_case("misleading-mce", 3, 40; "misleading mce 3")]
#[test_case("late-onset", 1, 60; "late onset 1")]
fn incremental_matches_fresh(machine: &str, seed: u64, boots: u32) {
    let events = simulated(machine, seed, boots);
    let mut state = State::new();
    let mut running = false;
    let mut checkpoints = 0;
    let mut questions = 0;
    for (i, event) in events.iter().enumerate() {
        state
            .fold(event)
            .unwrap_or_else(|err| panic!("{machine} {seed}: event {} {err}", event.seq.get()));
        running |= matches!(event.payload, Payload::TrialIntent(_));
        let last = i + 1 == events.len();
        let ask = running && !last && asked(&events[i + 1].payload);
        if !ask && i % CHECKPOINT != 0 && !last {
            continue;
        }
        let recomputed = fresh(&events[..=i]);
        if i % CHECKPOINT == 0 || last {
            assert!(
                state == recomputed,
                "{machine} {seed}: incremental state after event {} differs from a fresh fold",
                event.seq.get()
            );
            assert!(
                state.project() == recomputed.project(),
                "{machine} {seed}: incremental projection after event {} differs from a fresh fold",
                event.seq.get()
            );
            checkpoints += 1;
        }
        if ask {
            assert_eq!(
                state.next(),
                recomputed.next(),
                "{machine} {seed}: next after event {} differs from a fresh fold",
                event.seq.get()
            );
            questions += 1;
        }
    }
    assert!(
        questions > 0 && checkpoints > 1,
        "{machine} {seed}: {questions} questions, {checkpoints} checkpoints"
    );
}
