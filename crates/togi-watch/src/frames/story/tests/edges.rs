//! The narration's renderer-native edge cases (#904): every dead-end condition and stop reason, and every
//! premise with every uncertainty. They give the narration corpus the branches the simulated journals never
//! reach; each also asserts what the branch must say, so it stays a test.

use super::*;

fn dead_end(condition: DeadEndCondition, detail: &str) -> DeadEndView {
    DeadEndView {
        at: ts(10),
        condition,
        detail: detail.to_owned(),
    }
}

#[test]
fn every_dead_end_condition_is_narrated_with_and_without_a_saved_profile() {
    for &condition in DeadEndCondition::ALL {
        for saved in [false, true] {
            let mut fx = Fx::new();
            fx.dead_end = Some(dead_end(condition, "the recorded detail"));
            if saved {
                fx.stopped = Some(StopView {
                    at: ts(11),
                    reason: ShutdownReason::DeadEnd,
                    saved,
                });
            }
            let scene = fx.scene(ts(20));
            let said = story(&scene);
            assert_eq!(said.label, "DEAD END", "{condition:?}");
            assert!(!said.lines.is_empty(), "{condition:?}");
            let band = resting_band(&scene);
            assert_eq!(band.title, "DEAD END", "{condition:?}");
            assert_eq!(
                band.lines[0],
                format!(
                    "{}: the recorded detail",
                    condition.as_str().replace('_', " ")
                ),
                "{condition:?}"
            );
            assert_eq!(band.lines.len(), 1 + usize::from(saved), "{condition:?}");
        }
    }
}

#[test]
fn every_stop_reason_is_narrated_saved_or_not() {
    for &reason in ShutdownReason::ALL {
        for saved in [false, true] {
            let mut fx = Fx::new();
            fx.stopped = Some(StopView {
                at: ts(100),
                reason,
                saved,
            });
            let scene = fx.scene(ts(200));
            let said = story(&scene);
            assert_eq!(said.label, "STOPPED", "{reason:?} {saved}");
            assert!(!said.lines.is_empty(), "{reason:?} {saved}");
            let band = resting_band(&scene);
            assert_eq!(band.title, "STOPPED", "{reason:?} {saved}");
            assert_eq!(band.lines.len(), 1 + usize::from(saved), "{reason:?} {saved}");
        }
    }
}

fn running() -> Fx {
    let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0), (2, 1), (3, 1)]);
    fx.trial = Some(counted(
        in_cycle(trial(Regime::R1, Condition::Together, &[0, 1]), 1, 1, 1, 1),
        0,
        1,
        2,
    ));
    fx
}

/// Every premise under every uncertainty, with each decision the tuner records and each flag a branch carries,
/// and with each kind of next trial, in the outcome rows and in the lines fitted at three widths.
#[test]
fn every_premise_and_uncertainty_is_narrated_without_inventing_a_next_trial() {
    let nexts = [
        None,
        Some(plan(
            Regime::R1,
            many(&[0, 1]),
            Condition::Together,
            cycle_ctx(1, 2),
            120,
        )),
        Some(plan(
            Regime::R6,
            Load::Single(core(1)),
            Condition::Alone,
            cycle_ctx(1, 3),
            600,
        )),
        Some(plan(
            Regime::R7,
            many(&[0, 1]),
            Condition::Parked,
            cycle_ctx(1, 1),
            60,
        )),
    ];
    let premises = [
        Premise::Pass,
        Premise::AllPass,
        Premise::Named(core(0)),
        Premise::Named(core(3)),
        Premise::Unnamed,
        Premise::Inconclusive,
    ];
    let uncertainties = [
        None,
        Some(Uncertainty::NeedsRanking),
        Some(Uncertainty::NeedsHistory),
        Some(Uncertainty::RecurringPlan),
        Some(Uncertainty::Stopped { after: 0 }),
        Some(Uncertainty::Stopped { after: 2 }),
    ];
    let mut hold = backoff(1, -20, -20, None);
    hold.decision = Decision::Hold;
    let decisions: [Vec<DecisionPayload>; 4] = [
        Vec::new(),
        vec![DecisionPayload::TunerDecision(backoff(0, -31, -30, None))],
        vec![DecisionPayload::TunerDecision(hold)],
        vec![
            DecisionPayload::Failure(failure(Some(0))),
            DecisionPayload::TunerDecision(backoff(0, -31, -30, Some(-31))),
        ],
    ];
    let mut fx = running();
    for premise in premises {
        for uncertainty in uncertainties {
            for decision in &decisions {
                for next in &nexts {
                    for flags in 0..16_u8 {
                        let mut one = branch(premise);
                        one.uncertainty = uncertainty;
                        one.decisions.clone_from(decision);
                        one.next.clone_from(next);
                        one.at_zero = flags & 1 != 0;
                        one.without_telemetry = flags & 2 != 0;
                        one.offset_order = flags & 4 != 0;
                        one.top_requester = flags & 8 != 0;
                        fx.forecast.branches = vec![one];
                        let scene = fx.scene(ts(1010));
                        for row in outcome_rows(&scene) {
                            assert!(!row.label.is_empty(), "{premise:?} {uncertainty:?}");
                        }
                        for (width, compact) in [
                            (100, false),
                            (60, false),
                            (34, false),
                            (100, true),
                            (40, true),
                        ] {
                            drop(outcome_lines(&scene, width, compact));
                        }
                    }
                }
            }
        }
    }
}

/// The forecast between trials: each kind of next trial, the unranked uncertainty and nothing to say.
#[test]
fn a_forecast_between_trials_names_the_next_trial_the_ranking_or_nothing() {
    let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0), (2, 1), (3, 1)]);
    for next in [
        plan(Regime::R1, many(&[0, 1]), Condition::Together, cycle_ctx(1, 2), 120),
        plan(Regime::R6, Load::Single(core(1)), Condition::Alone, cycle_ctx(1, 3), 600),
        plan(Regime::R7, many(&[0, 1]), Condition::Parked, cycle_ctx(1, 1), 60),
    ] {
        fx.forecast.between = Some(Between {
            next: Some(next),
            ..Between::default()
        });
        let band = resting_band(&fx.scene(ts(1)));
        assert_eq!(band.title, "BETWEEN TRIALS");
        assert!(band.lines.iter().any(|line| line.starts_with("next: ")));
    }
    for uncertainty in [
        None,
        Some(Uncertainty::NeedsRanking),
        Some(Uncertainty::NeedsHistory),
        Some(Uncertainty::RecurringPlan),
        Some(Uncertainty::Stopped { after: 1 }),
    ] {
        fx.forecast.between = Some(Between {
            uncertainty,
            ..Between::default()
        });
        let band = resting_band(&fx.scene(ts(1)));
        assert!(band.lines.iter().any(|line| line.starts_with("next: ")));
    }
}
