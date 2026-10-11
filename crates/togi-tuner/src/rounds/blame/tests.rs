use togi_journal::Outcome;
use togi_machine::{CoreId, Regime};

use crate::testkit::{Journal, Start};
use crate::tests::core;
use crate::{Action, DecisionPayload};

fn depth(id: CoreId) -> i32 {
    if id == core(1) { -2 } else { -1 }
}

#[test]
fn a_multi_core_r7_failure_without_an_open_record_is_charged_by_depth_not_by_requests() {
    let mut j = Journal::new(&[0, 1], &[Start::solo(-10, -11), Start::solo(-10, -12)]);
    j.profile_change(&[-10, -10]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    for _ in 0..4000 {
        if j.run().phases.confirm_start().is_some() && j.run().checking.open().is_some() {
            break;
        }
        j.step(&mut |_| Outcome::Pass);
    }
    let plan = loop {
        match j.next() {
            Action::RunTrial(plan) if plan.regime == Regime::R7 => break plan,
            _ => {
                j.step(&mut |_| Outcome::Pass);
            }
        }
    };
    j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    assert!(matches!(attribution.payload, DecisionPayload::Failure(_)));
    let failure = j.decide(attribution);
    let run = j.run();
    let record = run
        .failures
        .pending(failure)
        .expect("the failure is pending");
    assert!(record.class.multi_r7());
    let deepened = record.class.loaded();
    assert!(run.r7.open.contains_key(&failure));
    assert_eq!(run.charged(record, failure, deepened, depth), Some(core(0)));
    assert!(!run.r7.open.contains_key(&record.ledger));
    assert_eq!(
        run.charged(record, record.ledger, deepened, depth),
        Some(core(1))
    );
}
