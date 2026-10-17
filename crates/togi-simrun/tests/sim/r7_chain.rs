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
        Some(Box::new(|event| {
            if let Payload::CheckingCycle(cycle) = &event.payload
                && cycle.event == CycleEvent::End
                && cycle.passed
            {
                Observe::Stop
            } else {
                Observe::Continue
            }
        })),
    );
    let mut got = Vec::new();
    let (mut chains, mut passes) = (0, 0);
    let (mut step, mut previous, mut last_end, mut last_pass) = (None, None, None, None);
    for event in &events {
        let payload = &event.payload;
        if let Payload::TrialIntent(intent) = payload
            && intent.regime == Regime::R7
            && intent.activity == Some(Activity::Checking)
        {
            got.push(intent.cores.iter().map(CoreId::get).collect::<Vec<_>>());
        } else if let Payload::TrialEnd(end) = payload {
            last_end = Some(event.seq);
            if end.outcome == Outcome::Pass {
                passes += 1;
                last_pass = Some(event.seq);
            }
        } else if let Payload::CheckingStep(_) = payload {
            step = Some(event.seq);
            passes = 0;
        } else if let Payload::CheckingChain(chain) = payload {
            chains += 1;
            assert!(chain.source_seqs.is_empty(), "{chain:?}");
            assert_eq!(
                passes, 4,
                "chain {chains} derived before its predecessor passed"
            );
            passes = 0;
            assert_eq!(
                last_end, last_pass,
                "chain {chains} followed a trial end that did not pass"
            );
            let step = step.expect("a checking step before its chains");
            let cited = last_pass.expect("a passing predecessor");
            assert_eq!(
                event.cause,
                [step, cited],
                "chain {chains} must cite its step and its predecessor's passing trial end"
            );
            // The cause names no earlier chain; the predecessor is linked through its passing trial end.
            assert!(
                previous.is_none_or(|previous| cited > previous),
                "chain {chains} cites a trial end from before the chain it follows: {cited:?}, previous {previous:?}"
            );
            previous = Some(event.seq);
            assert!(
                chain.cores.is_empty() || chain.cores.len() >= 2,
                "chain loaded fewer than two cores"
            );
        } else if let Payload::HuntStart(_) = payload {
            panic!("passing fallback chains started a hunt");
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
