use togi_journal::{HuntOutcome, Outcome};
use togi_machine::Regime;

use super::support::{ended, hunt_session, passed_cycle};
use crate::tests::core;
use crate::{Action, DecisionPayload};

#[test]
fn a_hunt_narrows_to_its_culprit_and_backs_it_off() {
    let mut j = hunt_session(&[-10, -10, -10, -10]);
    passed_cycle(&mut j, &[-5, -5, -5, -5], &[-10, -10, -10, -10]);
    j.together_failure(Regime::R6, 0, &[-10, -10, -10, -10]);
    let (start_seq, start) = j.start_hunt();
    assert_eq!(start.candidates.len(), 4);
    let culprit = core(2);
    let end = j.run_hunt(400, |plan| {
        if plan.profile.get(culprit).is_some_and(|o| o.get() <= -10) {
            Outcome::Failure
        } else {
            Outcome::Pass
        }
    });
    ended(&end, HuntOutcome::Culprit);
    assert_eq!(end.cores.first(), Some(culprit));
    let Action::Decide(commit) = j.next() else {
        panic!("expected the commitment");
    };
    assert!(
        matches!(commit.payload, DecisionPayload::TunerDecision(_)),
        "{commit:?}"
    );
    assert!(commit.cause[0] > start_seq);
    j.decide(commit);
    assert!(j.run().hunts.active.is_none());
    j.assert_resumes();
}
