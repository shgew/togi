//! The narration's renderer-native edge cases (#904): every dead-end condition and stop reason, and every
//! premise with every uncertainty. They give the narration corpus the branches the simulated journals never
//! reach; each also asserts what the branch must say, so it stays a test.

use super::super::Story;
use super::*;
use test_case::test_case;

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
            assert_eq!(
                band.lines.len(),
                1 + usize::from(saved),
                "{reason:?} {saved}"
            );
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
                        let rows = outcome_rows(&scene);
                        assert!(!rows.is_empty(), "{premise:?} {uncertainty:?} says nothing");
                        for row in &rows {
                            assert!(!row.label.is_empty(), "{premise:?} {uncertainty:?}");
                            if next.is_none() {
                                let said = fit_phrases(&row.phrases, 400);
                                assert!(
                                    !said.contains("next trial: "),
                                    "{premise:?} {uncertainty:?} invents a next trial: {said}"
                                );
                            }
                        }
                        for (width, compact) in [
                            (100, false),
                            (60, false),
                            (34, false),
                            (100, true),
                            (40, true),
                        ] {
                            let lines = outcome_lines(&scene, width, compact);
                            assert!(
                                !lines.is_empty(),
                                "{premise:?} {uncertainty:?} at {width} compact {compact}"
                            );
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
        plan(
            Regime::R1,
            many(&[0, 1]),
            Condition::Together,
            cycle_ctx(1, 2),
            120,
        ),
        plan(
            Regime::R6,
            Load::Single(core(1)),
            Condition::Alone,
            cycle_ctx(1, 3),
            600,
        ),
        plan(
            Regime::R7,
            many(&[0, 1]),
            Condition::Parked,
            cycle_ctx(1, 1),
            60,
        ),
    ] {
        fx.forecast.between = Some(Between {
            next: Some(next),
            ..Between::default()
        });
        let band = resting_band(&fx.scene(ts(1)));
        assert_eq!(band.title, "BETWEEN TRIALS");
        assert!(
            band.lines
                .iter()
                .any(|line| line.starts_with("next trial: "))
        );
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
        assert!(
            band.lines
                .iter()
                .any(|line| line.starts_with("next trial: "))
        );
    }
}

/// The cycle story says where the step sits whatever the journal gives it: no step number or a step number, and no
/// phase plan, phase 1 or phase 2.
#[test]
fn a_cycle_story_says_its_place_whatever_the_phase_plan_and_step_number_hold() {
    let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0)]);
    for (step, opening) in [(None, "This step is "), (Some(2), "Step 2 is ")] {
        let mut running = trial(Regime::R1, Condition::Together, &[0, 1]);
        running.cycle = Some(CyclePlace {
            cycle: 1,
            step,
            part: None,
            parts: 1,
        });
        fx.trial = Some(running);
        for (phase, place) in [
            (
                None,
                "Every core runs at its profile offset. The cycle keeps testing until stopped.",
            ),
            (
                Some(PhaseNo::One),
                "Every core runs at its profile offset. Phase 1's cycle: its pass confirms the first BIOS profile.",
            ),
            (
                Some(PhaseNo::Two),
                "Every core runs at its profile offset. Phase 2's confirmation cycle: its pass concludes phase 2.",
            ),
        ] {
            fx.phases = phase.map(|phase| PhasePlan {
                phase,
                phase1_end: None,
                concluded: None,
                confirming: false,
                rounds_left: 0,
                round: None,
                candidates: Vec::new(),
            });
            let told = story(&fx.scene(ts(1010)));
            assert_eq!(told.label, "CYCLE 1", "{step:?} {phase:?}");
            assert_eq!(told.lines.len(), 2, "{step:?} {phase:?}");
            assert!(
                told.lines[0].starts_with(opening),
                "{step:?} {phase:?}: {told:?}"
            );
            assert_eq!(told.lines[1], place, "{step:?} {phase:?}");
            assert!(
                told.brief.starts_with(opening),
                "{step:?} {phase:?}: {told:?}"
            );
        }
    }
}

fn hunting(cause: HuntCause) -> Fx {
    let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0), (2, 1)]);
    fx.hunt = Some((
        hunt_plan(),
        HuntFacts {
            started: Some(ts(990)),
            cause,
            parking: None,
            group_signals: Vec::new(),
        },
    ));
    fx.trial = Some(trial(Regime::R7, Condition::Together, &[0]));
    fx
}

fn told(cause: HuntCause) -> Story {
    let fx = hunting(cause);
    story(&fx.scene(ts(1010)))
}

/// Every cause a hunt can start from, with a core named and without, in the story and its compact line: an idle
/// crash or failure, a trial that failed with no signal, a trial whose length is not whole minutes, and a
/// failure already recorded with and without a regime or a carried trial.
#[test]
fn every_hunt_cause_is_narrated_with_and_without_a_named_core() {
    for named in [None, Some(core(0))] {
        let (opening, tail) = match named {
            None => ("No core was named: ", "; no core named."),
            Some(_) => ("Core 00 was named: ", "; core 00 named."),
        };
        let split = format!("{opening}I rerun that load with part of ");
        let common = |told: &Story| {
            assert_eq!(told.label, "HUNT 1");
            assert_eq!(told.lines.len(), 2, "{told:?}");
            assert!(told.lines[1].starts_with(&split), "{told:?}");
            assert!(
                told.lines[1].ends_with("at its offsets, the rest parked."),
                "{told:?}"
            );
            assert!(told.brief.contains(" Rerunning it with "), "{told:?}");
        };
        for (signal, first, brief) in [
            (
                None,
                "The machine crashed while idle, with no trial running, and rebooted.",
                "Crashed while idle",
            ),
            (
                Some(Signal::Crash),
                "The machine crashed while idle, with no trial running, and rebooted.",
                "Crashed while idle",
            ),
            (
                Some(Signal::ComputationError),
                "With no trial running and the offsets applied, the machine reported a wrong result.",
                "Wrong result while idle",
            ),
        ] {
            let mut idle = cause();
            idle.core = named;
            idle.signal = signal;
            let told = told(idle);
            common(&told);
            assert_eq!(told.lines[0], first, "{signal:?}");
            assert!(
                told.brief.starts_with(&format!("{brief}{tail} Rerunning")),
                "{signal:?}: {told:?}"
            );
        }
        for (carried, regime, first) in [
            (
                false,
                None,
                "An idle failure is already recorded at these offsets.",
            ),
            (
                true,
                None,
                "An idle failure is already recorded at these offsets, carried from an earlier session.",
            ),
            (
                false,
                Some(Regime::R7),
                "R7 all-core already failed at these offsets.",
            ),
            (
                true,
                Some(Regime::R7),
                "R7 all-core already failed at these offsets in a carried trial.",
            ),
        ] {
            let mut known = cause();
            known.core = named;
            known.known = true;
            known.carried = carried;
            known.regime = regime;
            let told = told(known);
            common(&told);
            assert_eq!(told.lines[0], first, "{carried} {regime:?}");
            assert!(
                told.brief.starts_with(&format!("{first} Rerunning")),
                "{carried} {regime:?}: {told:?}"
            );
        }
        for signal in [None, Some(Signal::Crash), Some(Signal::ComputationError)] {
            for (seconds, length) in [(45, "45-second"), (600, "10-minute")] {
                for rerun_of in [false, true] {
                    let mut ran = cause();
                    ran.core = named;
                    ran.signal = signal;
                    ran.rerun_of = rerun_of;
                    let mut failed = trial(Regime::R7, Condition::Together, &[0, 1]);
                    failed.duration = secs(seconds);
                    ran.trial = Some(failed);
                    let told = told(ran);
                    let what = if rerun_of {
                        format!("the rerun of the {length} all-core trial on ")
                    } else {
                        format!("the {length} all-core trial on ")
                    };
                    let case = format!("{signal:?} {seconds}s rerun {rerun_of}");
                    common(&told);
                    match signal {
                        None => {
                            assert!(
                                told.lines[0].starts_with(&capitalized(&what)),
                                "{case}: {told:?}"
                            );
                            assert!(told.lines[0].ends_with(" failed."), "{case}: {told:?}");
                            assert!(told.brief.starts_with("Failed in R7 all-core"), "{case}");
                        }
                        Some(Signal::Crash) => {
                            assert!(
                                told.lines[0].starts_with("The machine crashed during ")
                                    && told.lines[0].contains(&what)
                                    && told.lines[0].ends_with(" and rebooted."),
                                "{case}: {told:?}"
                            );
                            assert!(told.brief.starts_with("Crashed in R7 all-core"), "{case}");
                        }
                        Some(_) => {
                            assert!(
                                told.lines[0].starts_with(&capitalized(&what))
                                    && told.lines[0].ends_with(" ended with wrong result."),
                                "{case}: {told:?}"
                            );
                            assert!(
                                told.brief.starts_with("Wrong result in R7 all-core"),
                                "{case}"
                            );
                        }
                    }
                    assert!(
                        told.brief.contains(&format!("{tail} Rerunning")),
                        "{case}: {told:?}"
                    );
                }
            }
        }
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Draws `fx` as the dashboard shows it at the three main sizes; returns the rows of the widest.
fn drawn(fx: &Fx) -> Vec<String> {
    let mut widest = Vec::new();
    for (width, height) in [(240, 67), (160, 45), (120, 33)] {
        let snapshot = fx.snap();
        let journal = crate::screen::Journal::Ready {
            snapshot,
            log: snapshot.log(),
        };
        let sc = screen(View::Main, width, height, Colours::None, false);
        let (buf, _) = crate::frames::testing::render_checked(&journal, &sc, ts(1010));
        if widest.is_empty() {
            widest = rows_of(&buf);
        }
    }
    widest
}

fn hunt_group(number: u32, cores: &[u8], outcome: GroupState, carried: bool) -> HuntGroup {
    HuntGroup {
        number,
        cores: set(cores),
        profile: crate::tests::kit::offsets(&[]),
        stage: togi_journal::HuntStage::Full,
        probe: None,
        held: Vec::new(),
        outcome,
        passed: 0,
        needed: 5,
        carried,
        running: outcome == GroupState::Running,
    }
}

fn shows(rows: &[String], text: &str) {
    assert!(
        rows.iter().any(|row| row.contains(text)),
        "no row of the widest frame holds {text:?}:\n{}",
        rows.join("\n")
    );
}

/// The hunt panel: a group answered by existing evidence, a member probe running before its trial is counted,
/// and the evidence of a crash that names no core. Each is read off the widest frame; the narrower two are drawn
/// for the corpus and the bounds check.
#[test]
fn the_hunt_panel_says_carried_groups_uncounted_probes_and_the_evidence_of_a_crash() {
    let mut fx = hunting(cause());
    if let Some((plan, _)) = fx.hunt.as_mut() {
        plan.groups = vec![hunt_group(1, &[0], GroupState::Passed, true)];
    }
    shows(&drawn(&fx), "answered by existing evidence");

    let mut fx = hunting(cause());
    if let Some((plan, _)) = fx.hunt.as_mut() {
        plan.groups = vec![hunt_group(1, &[0, 1], GroupState::Failed, false)];
        plan.probes = vec![togi_tuner::MemberProbe {
            member: core(0),
            offset: off(-9),
            failed_at: Vec::new(),
            passed_at: Vec::new(),
            carried: Vec::new(),
            running: true,
            done: false,
        }];
    }
    let rows = drawn(&fx);
    shows(&rows, "► running");
    assert!(
        !rows.iter().any(|row| row.contains("► trial ")),
        "a probe with no trial count says which trial it is:\n{}",
        rows.join("\n")
    );

    let mut crashed = cause();
    crashed.signal = Some(Signal::Crash);
    crashed.trial = Some(trial(Regime::R7, Condition::Together, &[0, 1]));
    let mut end = end_view(Outcome::Failure, Some(Signal::Crash));
    end.last_sample = Some(secs(125));
    end.stalled = Some(core(1));
    crashed.end = Some(end);
    shows(
        &drawn(&hunting(crashed)),
        "core 01's worker had stalled by the last sample, 2:05 into the trial",
    );

    let mut idle_known = cause();
    idle_known.known = true;
    shows(
        &drawn(&hunting(idle_known)),
        "an idle failure is already recorded at these offsets",
    );
}

/// A cycle step that finished after two hunts.
#[test]
fn a_cycle_step_that_finished_after_two_hunts_lists_them() {
    let mut fx = Fx::new().with_cores(&[(0, 0), (1, 0), (2, 1), (3, 1)]);
    let mut first = step(Regime::R1, vec![part(&[0, 1], None, true, false)]);
    first.done = true;
    let second = step(Regime::R2, vec![part(&[0, 1], None, true, true)]);
    fx.cycle = Some((
        cycle_plan(1, vec![first, second], 1),
        vec![vec![1, 2], Vec::new()],
        None,
    ));
    fx.trial = Some(in_cycle(
        trial(Regime::R2, Condition::Together, &[0, 1]),
        1,
        2,
        1,
        1,
    ));
    shows(&drawn(&fx), "done · hunts 1-2");
}

/// Outcome rows the forecast merges: a zero core on each CCD, a pass and a failure that say the same, and a core
/// that starts its search over.
#[test]
fn outcome_rows_name_the_ccd_of_a_zero_core_and_merge_a_pass_with_a_failure() {
    let mut fx = running();
    let mut zero = [0, 3].map(|id| {
        let mut one = branch(Premise::Named(core(id)));
        one.at_zero = true;
        one
    });
    fx.forecast.branches = zero.to_vec();
    let scene = fx.scene(ts(1010));
    assert_eq!(zero_words(&scene, &zero[0]), "a CCD 0 core at 0");
    assert_eq!(zero_words(&scene, &zero[1]), "a CCD 1 core at 0");
    zero[0].top_requester = true;
    fx.forecast.branches = zero.to_vec();
    let scene = fx.scene(ts(1010));
    assert_eq!(zero_words(&scene, &zero[0]), "a top requester at 0");
    assert_eq!(
        zero_words(&scene, &zero[1]),
        "a core at 0",
        "a top requester at 0 does not move another CCD's group"
    );

    // Two named failures at 0 on different CCDs that say different things are two rows, each naming its CCD.
    let mut apart = [0, 3].map(|id| {
        let mut one = branch(Premise::Named(core(id)));
        one.at_zero = true;
        one
    });
    apart[0].decisions = vec![DecisionPayload::TunerDecision(backoff(1, -31, -30, None))];
    fx.forecast.branches = apart.to_vec();
    let labels: Vec<String> = fx.row_set().into_iter().map(|row| row.label).collect();
    assert_eq!(
        labels,
        [
            "if a CCD 0 core at 0 is named",
            "if a CCD 1 core at 0 is named"
        ],
        "{labels:?}"
    );
}

/// A pass and a failure at 0 that add nothing are one row; with a decision to share they still say something.
#[test_case(&[], Some("pass or fail"); "nothing to add is one row")]
#[test_case(
    &[DecisionPayload::TunerDecision(backoff(1, -31, -30, None))],
    None;
    "a shared backoff"
)]
fn a_pass_and_a_failure_at_zero_merge_only_when_they_add_nothing(
    decisions: &[DecisionPayload],
    merged: Option<&str>,
) {
    let mut fx = running();
    let mut pass = branch(Premise::Pass);
    decisions.clone_into(&mut pass.decisions);
    let mut failed = branch(Premise::Named(core(0)));
    decisions.clone_into(&mut failed.decisions);
    failed.at_zero = true;
    fx.forecast.branches = vec![pass, failed];
    let rows = fx.row_set();
    assert!(!rows.is_empty(), "{decisions:?}");
    if let Some(label) = merged {
        assert_eq!(
            rows.iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            [label],
            "a pass and a failure with nothing to add are one row"
        );
    }
}

#[test_case(Premise::Pass; "a pass")]
#[test_case(Premise::Unnamed; "an unnamed failure")]
#[test_case(Premise::Inconclusive; "an inconclusive trial")]
fn a_core_that_starts_its_search_over_is_named_under_every_premise(premise: Premise) {
    use crate::tests::kit::{Start, transition};
    let restart = Start {
        from: Some(togi_journal::CorePhase::AtLimit),
        to: togi_journal::CorePhase::Search,
        offset: -27,
        pass: None,
        fail: None,
    };
    let mut fx = running();
    let mut one = branch(premise);
    one.decisions = vec![DecisionPayload::CorePhase(transition(1, restart))];
    fx.forecast.branches = vec![one];
    let rows = fx.rows();
    assert!(
        rows.iter()
            .any(|row| row.contains("core 01 starts its search over")),
        "{premise:?}: {rows:?}"
    );
}
