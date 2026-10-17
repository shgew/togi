//! The narration's renderer-native edge cases (#904): every dead-end condition and stop reason, and every
//! premise with every uncertainty. They give the narration corpus the branches the simulated journals never
//! reach; each also asserts what the branch must say, so it stays a test.

use super::super::Story;
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
            assert_eq!(band.lines.len(), 1, "{condition:?} {saved}");
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
            assert_eq!(band.lines.len(), 1, "{reason:?} {saved}");
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

/// Draws `fx` as the dashboard shows it at the three main sizes on every page that tells about the work; returns
/// the rows of the widest size, the pages one after the other.
fn drawn(fx: &Fx) -> Vec<String> {
    let mut widest = Vec::new();
    for (width, height) in [(240, 67), (160, 45), (120, 33)] {
        for view in [
            View::Main,
            View::Cycle,
            View::R7,
            View::Hunts,
            View::History,
        ] {
            let snapshot = fx.snap();
            let journal = crate::screen::Journal::Ready {
                snapshot,
                log: snapshot.log(),
            };
            let sc = screen(view, width, height, Colours::None, false);
            let (buf, _) = crate::frames::testing::render_checked(&journal, &sc, ts(1010));
            if width == 240 {
                widest.extend(rows_of(&buf));
            }
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
