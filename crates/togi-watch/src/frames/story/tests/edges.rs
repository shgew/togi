//! The narration's renderer-native edge cases (#904): every dead-end condition and stop reason, and every
//! activity with every tested shape. They give the narration corpus the branches the simulated journals never
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
            assert_eq!(said.label, "DEAD END", "{condition:?} saved {saved}");
            assert!(!said.lines.is_empty(), "{condition:?} saved {saved}");
            let band = resting_band(&scene);
            assert_eq!(band.title, "DEAD END", "{condition:?} saved {saved}");
            assert_eq!(
                band.lines[0],
                format!(
                    "{}: the recorded detail",
                    condition.as_str().replace('_', " ")
                ),
                "{condition:?} saved {saved}"
            );
            assert_eq!(
                band.lines.len(),
                1 + usize::from(saved),
                "{condition:?} saved {saved}"
            );
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

/// Every night activity with every tested shape, retried or not, says what the trial is for and names its place.
#[test]
fn every_activity_is_narrated_with_every_tested_shape() {
    let shapes = [
        None,
        Some(Tested::Exact {}),
        Some(Tested::Isolated {
            ccd: ccd_id(1),
            offset: off(-15),
        }),
    ];
    for activity in [
        Activity::Mapping,
        Activity::Qualification,
        Activity::Reproduction,
        Activity::Control,
    ] {
        for tested in shapes {
            for retry in [false, true] {
                let mut fx = Fx::new();
                let mut running = trial(Regime::R1, &[0, 1]);
                running.activity = Some(activity);
                running.tested = tested;
                running.retry = retry;
                running.task = Some(task(Queue::Mapping, 2));
                fx.trial = Some(running);
                let said = story(&fx.scene(ts(1001)));
                let case = format!("{activity:?} {tested:?} {retry}");
                assert!(!said.label.is_empty(), "{case}");
                assert_eq!(said.lines.len(), 2, "{case}");
                assert!(said.lines.iter().all(|line| !line.is_empty()), "{case}");
                assert!(said.brief.contains("mapping task 2"), "{case}");
                assert_eq!(
                    said.lines[1].contains("retry of a trial that could not run"),
                    retry,
                    "{case}"
                );
            }
        }
    }
}
