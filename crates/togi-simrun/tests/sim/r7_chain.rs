use togi_journal::{Activity, CycleEvent, Outcome, Payload};
use togi_machine::{CoreId, Regime};
use togi_simrun::Observe;

use crate::common::Bench;
use crate::common::hunt_machine;
use crate::hunt::{config, run, sharp_limits};

fn loaded(first: u8, count: u8) -> Vec<u8> {
    (first..first + count).collect()
}

#[test]
fn simulation_runs_full_first_offset_fallback_chains() {
    let mut definition = hunt_machine(16);
    sharp_limits(&mut definition);
    let candidates: Vec<(i64, i64)> = (0..16).map(|id| (id, -10 - 5 * (id % 8))).collect();
    let bench = Bench::new(&definition, 1);
    let (_, events) = run(
        &bench,
        config(&candidates, Some(&[Regime::R7])),
        Some(Box::new(|event| match &event.payload {
            Payload::CheckingCycle(cycle) if cycle.event == CycleEvent::End && cycle.passed => {
                Observe::Stop
            }
            _ => Observe::Continue,
        })),
    );
    let mut got = Vec::new();
    let (mut chains, mut passes) = (0, 0);
    let (mut step, mut previous) = (None, None);
    for event in &events {
        match &event.payload {
            Payload::TrialIntent(intent)
                if intent.regime == Regime::R7 && intent.activity == Some(Activity::Checking) =>
            {
                got.push(intent.cores.iter().map(CoreId::get).collect::<Vec<_>>());
            }
            Payload::TrialEnd(end) if end.outcome == Outcome::Pass => passes += 1,
            Payload::CheckingStep(_) => {
                step = Some(event.seq);
                passes = 0;
            }
            Payload::CheckingChain(chain) => {
                chains += 1;
                assert!(chain.source_seqs.is_empty(), "{chain:?}");
                assert!(event.msg.contains("offset fallback"), "{}", event.msg);
                assert_eq!(
                    passes, 4,
                    "chain {chains} derived before its predecessor passed"
                );
                passes = 0;
                assert!(
                    step.is_some_and(|step| event.cause.contains(&step)),
                    "chain {chains} does not cite its step: cause {:?}, step {step:?}",
                    event.cause
                );
                assert!(
                    previous.is_none_or(|previous| event.cause.contains(&previous)),
                    "chain {chains} does not cite the chain before it: cause {:?}, previous {previous:?}",
                    event.cause
                );
                previous = Some(event.seq);
                assert!(
                    chain.cores.is_empty() || chain.cores.len() >= 2,
                    "chain loaded fewer than two cores"
                );
            }
            Payload::HuntStart(_) => panic!("passing fallback chains started a hunt"),
            _ => {}
        }
    }
    let mut want = Vec::new();
    for base in [0, 8] {
        for count in (2..=8).rev() {
            for _ in 0..4 {
                want.push(loaded(base + 8 - count, count));
            }
        }
    }
    for _ in 0..4 {
        want.push(loaded(0, 16));
    }
    assert_eq!(got, want);
    assert_eq!(chains, 14);
}
