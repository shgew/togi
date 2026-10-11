use togi_journal::{CycleEvent, Outcome};
use togi_machine::{Condition, Regime};

use crate::checking::Peak;
use crate::testkit::{Journal, config};
use crate::tests::core;
use crate::{Action, DecisionPayload, Load, TrialPlan};

fn started(offsets: &[i64]) -> Journal {
    let mut j = Journal::has_room(offsets);
    let change = j.next_decision();
    j.decide(change);
    j
}

fn with_tctl(
    j: &mut Journal,
    plan: &TrialPlan,
    outcome: Outcome,
    celsius: Option<i32>,
) -> togi_journal::Seq {
    let (intent, trial) = j.intent(plan);
    let signal = (outcome == Outcome::Failure).then_some(togi_machine::Signal::ComputationError);
    let mut end = Journal::end_payload(&trial, outcome, signal);
    end.tctl_max_c = celsius;
    j.append(end.into(), &[intent])
}

fn r6(j: &Journal) -> TrialPlan {
    j.manual(
        Regime::R6,
        0,
        Load::Many(crate::testkit::set(&[0, 1])),
        Condition::Together,
        &[-10, -12],
        j.run().config.checking_idle.get(),
    )
}

#[test]
fn unpassed_cycle_steps_done_replays() {
    let mut j = Journal::has_room(&[-10, -12]);
    j.load(&config(r#"{"checking":{"cycle":["R1","R2"]}}"#));
    let change = j.next_decision();
    j.decide(change);
    while j
        .run()
        .checking
        .open()
        .is_none_or(|open| open.steps_done() < 1)
    {
        j.step(&mut |_| Outcome::Pass);
    }
    let number = j.run().checking.open().unwrap().number.get();
    j.cycle_end(number, false, false);
    let closed = j.run().checking.closed().unwrap();
    assert_eq!(closed.steps_done, 1);
    assert_eq!(closed.steps.len(), 2);
    j.assert_resumes();
}

#[test]
fn a_passed_cycle_closes_with_every_step_done() {
    let mut j = Journal::has_room(&[-10, -12]);
    j.load(&config(r#"{"checking":{"cycle":["R1","R2"]}}"#));
    let change = j.next_decision();
    j.decide(change);
    loop {
        let action = j.step(&mut |_| Outcome::Pass);
        if matches!(&action, Action::Decide(d) if matches!(&d.payload, DecisionPayload::CheckingCycle(c) if c.event == CycleEvent::End))
        {
            break;
        }
    }
    assert_eq!(j.run().checking.closed().unwrap().steps_done, 2);
    assert!(j.run().checking.open().is_none());
    j.assert_resumes();
}

#[test]
fn temperature_peak_since_last_profile_change() {
    let mut j = started(&[-10, -12]);
    let plan = r6(&j);
    let peak = with_tctl(&mut j, &plan, Outcome::Pass, Some(92));
    assert_eq!(
        j.run().checking.temperature_peak(),
        Some(Peak {
            celsius: 92,
            seq: peak
        })
    );
    j.profile_change(&[-9, -12]);
    assert_eq!(j.run().checking.temperature_peak(), None);
    let next = with_tctl(&mut j, &plan, Outcome::Pass, Some(81));
    j.idle_failure(j.profile(&[-9, -12]).offsets(), Condition::Together);
    assert_eq!(
        j.run().checking.temperature_peak(),
        Some(Peak {
            celsius: 81,
            seq: next
        })
    );
    j.assert_resumes();
}

#[test]
fn temperature_counts_only_together_passes_with_a_reading() {
    let mut j = started(&[-10, -12]);
    j.profile_change(&[-10, -12]);
    let plan = r6(&j);
    with_tctl(&mut j, &plan, Outcome::Pass, None);
    with_tctl(&mut j, &plan, Outcome::Inconclusive, Some(98));
    with_tctl(&mut j, &plan, Outcome::Failure, Some(99));
    let attribution = j.next_decision();
    j.decide(attribution);
    assert_eq!(j.run().checking.temperature_peak(), None);

    let mut alone = Journal::search_at(&[-10]);
    let search = alone.next_trial();
    let (intent, trial) = alone.intent(&search);
    let mut end = Journal::end_payload(&trial, Outcome::Pass, None);
    end.tctl_max_c = Some(99);
    alone.append(end.into(), &[intent]);
    assert_eq!(alone.run().checking.temperature_peak(), None);

    let first = with_tctl(&mut j, &plan, Outcome::Pass, Some(84));
    with_tctl(&mut j, &plan, Outcome::Pass, Some(84));
    assert_eq!(
        j.run().checking.temperature_peak(),
        Some(Peak {
            celsius: 84,
            seq: first
        })
    );
    let _ = core(0);
}
