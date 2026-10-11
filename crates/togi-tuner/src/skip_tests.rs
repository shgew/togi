use togi_journal::{Attribution, Outcome, Payload};
use togi_machine::{Condition, Regime};

use crate::testkit::{Journal, Start, set};
use crate::tests::core;
use crate::{Action, DecisionPayload, Load, TrialPlan};

const SESSION: &str = "20261002T004254Z";

fn skipped(j: &Journal, plan: &TrialPlan) -> Action {
    j.run().skip_known_failure(Action::RunTrial(plan.clone()))
}

fn known_failure(action: &Action) -> Option<(togi_journal::Failure, Vec<togi_journal::Seq>)> {
    match action {
        Action::Decide(decision) => match &decision.payload {
            DecisionPayload::Failure(failure) => Some((failure.clone(), decision.cause.clone())),
            _ => None,
        },
        _ => None,
    }
}

#[test]
fn skip_known_failure() {
    struct Case {
        name: &'static str,
        live: bool,
        profile: [i64; 2],
        passes: i32,
        change: fn(&mut TrialPlan),
        skip: bool,
    }
    fn same(_: &mut TrialPlan) {}
    let cases = [
        Case {
            name: "carried equal",
            live: false,
            profile: [-20, -20],
            passes: 0,
            change: same,
            skip: true,
        },
        Case {
            name: "live equal",
            live: true,
            profile: [-20, -20],
            passes: 0,
            change: same,
            skip: true,
        },
        Case {
            name: "shallower",
            live: false,
            profile: [-19, -20],
            passes: 0,
            change: same,
            skip: true,
        },
        Case {
            name: "one fewer covering passes",
            live: false,
            profile: [-20, -20],
            passes: -1,
            change: same,
            skip: true,
        },
        Case {
            name: "covered",
            live: false,
            profile: [-20, -20],
            passes: 1,
            change: same,
            skip: false,
        },
        Case {
            name: "deeper",
            live: false,
            profile: [-21, -20],
            passes: 0,
            change: same,
            skip: false,
        },
        Case {
            name: "incomparable",
            live: false,
            profile: [-19, -21],
            passes: 0,
            change: same,
            skip: false,
        },
        Case {
            name: "regime",
            live: false,
            profile: [-20, -20],
            passes: 0,
            change: |plan| {
                plan.regime = Regime::R1;
                plan.workload =
                    crate::WorkloadChoice::Frozen(togi_machine::workloads(Regime::R1)[0].id());
            },
            skip: false,
        },
        Case {
            name: "loaded cores",
            live: false,
            profile: [-20, -20],
            passes: 0,
            change: |plan| plan.load = Load::Many(set(&[0])),
            skip: false,
        },
        Case {
            name: "duration",
            live: false,
            profile: [-20, -20],
            passes: 0,
            change: |plan| {
                plan.duration =
                    crate::Secs::new(std::num::NonZeroU32::new(plan.duration.get() + 1).unwrap());
            },
            skip: false,
        },
    ];
    for case in cases {
        let mut j = Journal::new(&[0, 1], &[Start::at_limit(-20), Start::at_limit(-20)]);
        j.profile_change(&[-20, -20]);
        let duration = j.run().config.short_trial.get();
        let mut plan = j.manual(
            Regime::R6,
            0,
            Load::Many(set(&[1, 0])),
            Condition::Together,
            &[-20, -20],
            duration,
        );
        let seq = if case.live {
            let mut old = plan.clone();
            old.profile = j.profile(&case.profile);
            let end = j.trial(&old, Outcome::Failure);
            let attribution = j.next_decision();
            j.decide(attribution);
            end
        } else {
            j.carry(
                SESSION,
                (Regime::R6, 0, &[0, 1], duration),
                Condition::Together,
                &case.profile,
                Outcome::Failure,
            )
        };
        if case.passes != 0 {
            let count = usize::from(j.run().config.needed.get()) - usize::from(case.passes < 0);
            for _ in 0..count {
                j.carry(
                    SESSION,
                    (Regime::R6, 0, &[0, 1], duration),
                    Condition::Together,
                    &[-20, -20],
                    Outcome::Pass,
                );
            }
        }
        (case.change)(&mut plan);
        let action = skipped(&j, &plan);
        if !case.skip {
            assert!(
                matches!(action, Action::RunTrial(_)),
                "{}: {action:?}",
                case.name
            );
            continue;
        }
        let (failure, cause) =
            known_failure(&action).unwrap_or_else(|| panic!("{}: {action:?}", case.name));
        assert_eq!(failure.known_failure, Some(seq), "{}", case.name);
        assert_eq!(cause, [seq], "{}", case.name);
        let text = Payload::from(DecisionPayload::Failure(failure)).message();
        assert!(text.contains("skipped"), "{}: {text}", case.name);
    }
}

#[test]
fn a_known_single_core_r7_failure_matches_its_workload() {
    let mut j = Journal::has_room(&[-20, -20]);
    let duration = j.run().config.short_trial.get();
    let fact = j.carry(
        SESSION,
        (Regime::R7, 0, &[0], duration),
        Condition::Together,
        &[-20, -20],
        Outcome::Failure,
    );
    let mut plan = j.manual(
        Regime::R7,
        0,
        Load::Many(set(&[0])),
        Condition::Together,
        &[-20, -20],
        duration,
    );
    let (failure, _) =
        known_failure(&skipped(&j, &plan)).expect("the known failure skips the trial");
    assert_eq!(failure.known_failure, Some(fact));
    plan = j.manual(
        Regime::R7,
        1,
        Load::Many(set(&[0])),
        Condition::Together,
        &[-20, -20],
        duration,
    );
    assert!(matches!(skipped(&j, &plan), Action::RunTrial(_)));
}

#[test]
fn a_multi_core_r7_trial_is_never_skipped() {
    let mut j = Journal::has_room(&[-20, -20]);
    let duration = j.run().config.short_trial.get();
    j.carry(
        SESSION,
        (Regime::R7, 0, &[0, 1], duration),
        Condition::Together,
        &[-20, -20],
        Outcome::Failure,
    );
    let plan = j.manual(
        Regime::R7,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Together,
        &[-20, -20],
        duration,
    );
    assert!(matches!(skipped(&j, &plan), Action::RunTrial(_)));
}

#[test]
fn a_known_alone_failure_uses_the_recorded_offset() {
    let mut j = Journal::search_at(&[-20]);
    let duration = j.run().config.search_trial.get();
    let fact = j.carry(
        SESSION,
        (Regime::R1, 0, &[0], duration),
        Condition::Alone,
        &[-19],
        Outcome::Failure,
    );
    let action = j.next();
    let (failure, cause) = known_failure(&action).expect("the skip is a failure");
    assert_eq!(failure.attribution, Attribution::Attributed);
    assert_eq!(failure.core, Some(core(0)));
    assert_eq!(
        failure.offset.map(togi_machine::TargetOffset::get),
        Some(-19)
    );
    assert_eq!((failure.known_failure, cause), (Some(fact), vec![fact]));
}

#[test]
fn a_known_together_failure_infers_the_sole_nonzero_core() {
    let mut j = Journal::has_room(&[0, -20]);
    let fact = j.carry(
        SESSION,
        (Regime::R6, 0, &[0, 1], 120),
        Condition::Together,
        &[0, -19],
        Outcome::Failure,
    );
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Together,
        &[0, -20],
        120,
    );
    let action = skipped(&j, &plan);
    let (failure, _) = known_failure(&action).expect("the known failure skips the trial");
    assert_eq!(failure.attribution, Attribution::Attributed);
    assert_eq!(failure.core, Some(core(1)));
    assert_eq!(
        failure.offset.map(togi_machine::TargetOffset::get),
        Some(-19)
    );
    assert_eq!(failure.known_failure, Some(fact));
    let Action::Decide(decision) = action else {
        unreachable!()
    };
    let seq = j.decide(decision);
    assert_eq!(j.run().core(core(1)).pending, Some(seq));
    assert!(j.run().flight.awaiting.is_none());
    j.assert_resumes();
}

#[test]
fn a_skipped_failure_adds_no_evidence() {
    let mut j = Journal::search_at(&[-20]);
    let duration = j.run().config.search_trial.get();
    j.carry(
        SESSION,
        (Regime::R1, 0, &[0], duration),
        Condition::Alone,
        &[-19],
        Outcome::Failure,
    );
    let before = j.run().evidence.clone();
    let action = j.next_decision();
    j.decide(action);
    assert_eq!(j.run().evidence, before);
    assert!(matches!(
        j.next_decision().payload,
        DecisionPayload::TunerDecision(_)
    ));
}
