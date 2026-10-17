//! What the frame says about the session's state: the story, the NOW band and the stage line, whole.

use ratatui::buffer::Buffer;
use test_case::test_case;
use togi_journal::{
    CommandReset, DeadEnd, DeadEndCondition, EventKind, Outcome, ResetTarget, Shutdown,
    ShutdownReason, TrialProgress,
};
use togi_machine::Signal;
use togi_watchtest::{Scenario, cut_at};

use super::{ALL_SIZES, Fixture, SIZES, append, later, main_screen, one_frame, text, words};
use crate::frames::layout::Layout;
use crate::frames::terms::dead_end_words;
use crate::frames::testing::rows_of;
use crate::screen::{Hold, Screen, View};
use crate::tests::kit::{Script, crash, end_payload, warning};

pub(super) fn stage_line(buf: &Buffer, screen: &Screen) -> String {
    rows_of(buf)[usize::from(Layout::new(screen).stage.y)]
        .trim()
        .to_owned()
}

pub(super) fn summary_screen() -> Screen {
    Screen {
        view: View::Log,
        ..main_screen(240, 67)
    }
}

fn session_events() -> Vec<togi_journal::Event> {
    Scenario::Stopped
        .base()
        .events()
        .expect("the simulated session")
        .to_vec()
}

#[test]
fn between_trials_names_the_last_outcome_and_the_next_action_not_a_countdown() {
    let fx = Fixture::scenario(Scenario::Between);
    assert!(fx.model.snapshot().trial.is_none() && fx.model.snapshot().last.is_some());
    for (width, height) in SIZES {
        let (buf, drawn) = fx.main(width, height);
        let shown = text(&buf);
        assert!(
            shown.contains("BETWEEN TRIALS"),
            "{width}x{height} loses its state:\n{shown}"
        );
        assert!(
            !shown.contains("left"),
            "{width}x{height} invents a countdown:\n{shown}"
        );
        let lower = shown.to_lowercase();
        assert!(
            lower.contains("next trial:") || lower.contains("deciding"),
            "{width}x{height} hides the tuner's next action:\n{shown}"
        );
        assert_eq!(drawn.hold, Hold::Never);
    }
}

#[test]
fn a_stopped_frame_shows_saved_tune_against_the_offset_now_and_no_running_stage() {
    let fx = Fixture::scenario(Scenario::Stopped);
    assert!(fx.model.snapshot().stopped.is_some_and(|stop| stop.saved));
    for (width, height) in ALL_SIZES {
        let (buf, _) = fx.main(width, height);
        let shown = text(&buf).to_lowercase();
        assert!(
            shown.contains("stopped") && shown.contains("tune") && shown.contains("now"),
            "{width}x{height} hides the saved tune against the offset now:\n{shown}"
        );
        let stage = stage_line(&buf, &main_screen(width, height));
        assert!(
            !stage.contains('►') && !stage.contains("repeats until stopped"),
            "{width}x{height}: a stopped stage line shows a running stage: {stage:?}"
        );
    }
    let (wide, _) = fx.main(240, 67);
    let said = words(&wide);
    assert!(
        said.contains("requested clean cycles completed"),
        "the stop sentence says why the session stopped: {said}"
    );
}

#[test]
fn a_running_stage_line_keeps_its_marker_and_its_repeat_note() {
    let fx = Fixture::scenario(Scenario::Concluded);
    assert!(fx.model.snapshot().stopped.is_none());
    let (buf, _) = fx.main(240, 67);
    let stage = stage_line(&buf, &main_screen(240, 67));
    assert!(
        stage.contains('►') && stage.contains("repeats until stopped"),
        "{stage:?}"
    );
}

#[test_case(
    DeadEndCondition::NoEvidence,
    "kernel log unreadable: permission denied",
    "installed";
    "no evidence"
)]
#[test_case(
    DeadEndCondition::Containment,
    "termination unconfirmed",
    "termination confirmed";
    "containment"
)]
#[test_case(
    DeadEndCondition::ThermalTrip,
    "firmware reported a thermal trip",
    "wrong result";
    "thermal trip"
)]
fn a_dead_end_frame_keeps_the_recorded_cause_and_invents_no_diagnosis(
    condition: DeadEndCondition,
    detail: &str,
    wrong: &str,
) {
    let mut script = Script::at_limit();
    script.append(
        DeadEnd {
            condition,
            core: None,
            detail: detail.to_owned(),
            action: None,
        }
        .into(),
        &[],
    );
    let fx = Fixture::of(&script.events);
    let (buf, _) = fx.main(240, 67);
    let shown = text(&buf);
    assert!(
        shown.contains("DEAD END") && shown.contains(detail) && !shown.contains(wrong),
        "a dead end loses its recorded cause or invents one:\n{shown}"
    );
}

#[test]
fn a_dead_end_frame_says_why_tuning_cannot_continue_for_every_condition() {
    for &condition in DeadEndCondition::ALL {
        let mut script = Script::at_limit();
        script.append(
            DeadEnd {
                condition,
                core: None,
                detail: "the recorded detail".to_owned(),
                action: None,
            }
            .into(),
            &[],
        );
        let (buf, _) = Fixture::of(&script.events).main(240, 67);
        let shown = text(&buf);
        assert!(
            shown.contains(dead_end_words(condition)),
            "a {condition:?} dead end hides why tuning cannot continue:\n{shown}"
        );
    }
}

fn trial_events() -> (Vec<togi_journal::Event>, usize) {
    let events = session_events();
    let first_intent = events
        .iter()
        .position(|event| event.kind() == EventKind::TrialIntent)
        .expect("a trial intent");
    (events, first_intent)
}

#[test]
fn a_session_with_no_trial_yet_does_not_claim_one_ended() {
    let (events, first_intent) = trial_events();
    let fx = Fixture::of(&events[..first_intent]);
    assert!(fx.model.snapshot().trial.is_none());
    for (width, height) in SIZES {
        let said = words(&fx.main(width, height).0);
        assert!(
            said.contains("No trial has run yet"),
            "{width}x{height} lacks the no-trial words: {said}"
        );
        assert!(
            !said.contains("has ended"),
            "{width}x{height} claims a trial ended: {said}"
        );
    }
}

#[test]
fn a_first_trial_that_ended_is_reported_as_the_last_one() {
    let (events, _) = trial_events();
    let ended = cut_at(&events, |event| event.kind() == EventKind::TrialEnd).expect("a trial end");
    let fx = Fixture::of(ended);
    assert!(fx.model.snapshot().trial.is_none());
    for (width, height) in SIZES {
        let said = words(&fx.main(width, height).0);
        assert!(
            said.contains("last: PASS"),
            "{width}x{height} lacks \"last: PASS\": {said}"
        );
    }
}

#[test]
fn a_crashed_trial_that_a_later_boot_outlived_is_not_narrated_as_running() {
    let fx = Fixture::scenario(Scenario::Crashed);
    let id = fx
        .model
        .snapshot()
        .trial
        .expect("a crashed trial")
        .id
        .clone();
    assert!(fx.model.snapshot().crashed_trial());
    for (width, height) in SIZES {
        let (buf, drawn) = fx.main(width, height);
        let said = words(&buf);
        for want in [
            "CRASHED",
            "togi run records the crash",
            &format!("trial {id}"),
        ] {
            assert!(
                said.contains(want),
                "{width}x{height} lacks {want:?}: {said}"
            );
        }
        for bad in ["left", "past its end", "I'm finding", "I'm confirming"] {
            assert!(
                !said.contains(bad),
                "{width}x{height} says {bad:?} of a crashed trial: {said}"
            );
        }
        assert_eq!(drawn.hold, Hold::Never);
    }
    let summary = summary_screen();
    let (wide, _) = fx.draw(&summary, fx.now);
    let stage = stage_line(&wide, &summary);
    assert!(
        !stage.contains('►') && !stage.contains("left") && stage.contains("trial crashed"),
        "{stage:?}"
    );
    let started = Fixture::scenario(Scenario::Search);
    assert!(
        !started.model.snapshot().crashed_trial(),
        "a trial in the latest boot is running"
    );
}

#[test]
fn a_trial_past_its_planned_end_shows_how_long_without_claiming_a_crash() {
    let fx = Fixture::scenario(Scenario::Search);
    let trial = fx.model.snapshot().trial.expect("a started trial");
    let end = later(
        trial.started.expect("a started trial"),
        i64::try_from(trial.duration.as_secs()).expect("seconds"),
    );
    for (width, height) in SIZES {
        let sc = main_screen(width, height);
        let before = words(&fx.draw(&sc, later(end, -1)).0);
        let required = trial.duration.as_secs();
        let clock = |secs: u64| format!("{}:{:02}", secs / 60, secs % 60);
        assert!(
            before.contains(&format!("{} / {}", clock(required - 1), clock(required)))
                && !before.contains("left")
                && !before.contains("past"),
            "{width}x{height} before the planned end: {before}"
        );
        let (buf, drawn) = fx.draw(&sc, later(end, 42));
        let said = words(&buf);
        for want in ["0:42", "past", "may still be running"] {
            assert!(
                said.contains(want),
                "{width}x{height} lacks {want:?}: {said}"
            );
        }
        for bad in ["0:00 left", "crashed", "togi run records", "CRASHED"] {
            assert!(
                !said.contains(bad),
                "{width}x{height} says {bad:?} past the planned end: {said}"
            );
        }
        assert_eq!(drawn.hold, Hold::Never, "{width}x{height} keeps ticking");
    }
    let summary = summary_screen();
    let (wide, _) = fx.draw(&summary, later(end, 42));
    assert!(
        stage_line(&wide, &summary).contains("0:42 past its end"),
        "the stage line says how long past its end"
    );
}

#[test]
fn a_trial_overdue_with_later_progress_reports_its_missing_end() {
    let mut events = Scenario::Search.events().expect("the scenario's journal");
    let fx = Fixture::of(&events);
    let trial = fx.model.snapshot().trial.expect("a started trial");
    let end = later(
        trial.started.expect("started"),
        i64::try_from(trial.duration.as_secs()).expect("seconds"),
    );
    let id = trial.id.clone();
    let last = events.last().expect("an event").time;
    let boot = events.last().expect("an event").boot.to_string();
    append(
        &mut events,
        TrialProgress {
            trial: id,
            detail: "FFT 36K done".to_owned(),
            signal: None,
            core: None,
        }
        .into(),
        end.duration_since(last).as_secs() + 10,
        &boot,
    );
    let fx = Fixture::of(&events);
    let now = later(end, 42);
    for (width, height) in SIZES {
        for sc in [main_screen(width, height), one_frame(width, height)] {
            let (buf, drawn) = fx.draw(&sc, now);
            let said = words(&buf);
            for want in [
                "0:42",
                "past",
                "no trial end recorded",
                "may still be running",
            ] {
                assert!(
                    said.contains(want),
                    "{width}x{height} lacks {want:?}: {said}"
                );
            }
            for bad in [
                "nothing recorded since",
                "0:00 left",
                "crashed",
                "CRASHED",
                "togi run records the crash",
            ] {
                assert!(
                    !said.contains(bad),
                    "{width}x{height} says {bad:?} despite later progress: {said}"
                );
            }
            assert_eq!(drawn.hold, Hold::Never);
        }
    }
}

#[test]
fn a_prior_recovery_does_not_mask_a_later_boot_crash() {
    let mut script = Script::at_limit();
    let plan = script.next_trial();
    let (intent, first) = script.intent(&plan);
    script.start(&first);
    script.boot = "next".to_owned();
    let crashed = script.append(crash("boot", Some(intent)).into(), &[]);
    script.append(
        end_payload(&first, Outcome::Failure, Some(Signal::Crash)).into(),
        &[crashed],
    );
    let plan = script.next_trial();
    let (_, second) = script.intent(&plan);
    let preparing = script.model();
    let snapshot = preparing.snapshot();
    assert!(
        snapshot
            .trial
            .is_some_and(|trial| trial.id == second && !trial.crashed)
    );
    assert!(
        snapshot.recover.is_some(),
        "the first trial's recovery is current"
    );
    script.boot = "later".to_owned();
    script.append(warning("a later boot wrote this"), &[]);
    let fx = Fixture::of(&script.events);
    assert!(
        fx.model
            .snapshot()
            .trial
            .is_some_and(|trial| trial.id == second && trial.crashed)
    );
    for (width, height) in SIZES {
        for sc in [main_screen(width, height), one_frame(width, height)] {
            let said = words(&fx.draw(&sc, fx.now).0);
            for want in [
                "CRASHED, NOT YET RECOVERED",
                &format!("trial {second}"),
                "togi run records the crash",
            ] {
                assert!(
                    said.contains(want),
                    "one_frame {} {width}x{height} lacks {want:?}: {said}",
                    sc.one_frame
                );
            }
            for bad in [
                "RECOVERED FROM A CRASH",
                "I picked up from the journal after the reboot",
            ] {
                assert!(
                    !said.contains(bad),
                    "{width}x{height} presents the first trial's recovery as current: {said}"
                );
            }
        }
    }
}

#[test]
fn a_command_shutdown_does_not_mask_an_unrecovered_crash() {
    let mut events = Scenario::Search.events().expect("the scenario's journal");
    let running = Fixture::of(&events);
    let trial = running
        .model
        .snapshot()
        .trial
        .expect("a started trial")
        .clone();
    assert!(!trial.crashed && running.model.snapshot().recover.is_none());
    let core = trial.core.expect("the trial's core");
    append(
        &mut events,
        CommandReset {
            target: ResetTarget::Core(core),
        }
        .into(),
        1,
        "reset-boot",
    );
    append(
        &mut events,
        Shutdown {
            reason: ShutdownReason::Command,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }
        .into(),
        1,
        "reset-boot",
    );
    let fx = Fixture::of(&events);
    let snapshot = fx.model.snapshot();
    assert!(snapshot.trial.is_some_and(|crashed| crashed.crashed));
    assert!(snapshot.stopped.is_some() && snapshot.recover.is_none());
    for (width, height) in SIZES {
        for sc in [main_screen(width, height), one_frame(width, height)] {
            let (buf, _) = fx.draw(&sc, fx.now);
            let said = words(&buf);
            for want in [
                "CRASHED, NOT YET RECOVERED",
                &format!("trial {}", trial.id),
                "togi run records the crash",
            ] {
                assert!(
                    said.contains(want),
                    "{width}x{height} lacks {want:?}: {said}"
                );
            }
            for bad in ["left", "past its end", "RECOVERED FROM A CRASH"] {
                assert!(
                    !said.contains(bad),
                    "{width}x{height} says {bad:?} instead of the unrecovered crash: {said}"
                );
            }
        }
    }
    let summary = summary_screen();
    let (wide, _) = fx.draw(&summary, fx.now);
    let stage = stage_line(&wide, &summary);
    assert!(
        stage.contains("STOPPED") && !stage.contains('►') && !stage.contains("left"),
        "a reset after a reboot presents a running stage: {stage:?}"
    );
}

#[test]
fn a_rerun_that_completes_the_cycle_names_the_step_checking_resumes_at_never_the_next() {
    let built = togi_watchtest::cycle_completed_by_rerun().expect("the rerun journals");
    assert!(!built.running.is_empty());
    for (n, events) in built.running.iter().enumerate() {
        let fx = Fixture::of(events);
        let (wide, _) = fx.main(240, 67);
        let cycle = Screen {
            view: View::Cycle,
            ..main_screen(240, 67)
        };
        let said = format!("{} {}", words(&wide), words(&fx.draw(&cycle, fx.now).0));
        assert!(
            said.contains("checking resumes at step 2"),
            "rerun {}: {said}",
            n + 1
        );
        assert!(
            !said.contains("resumes at step 1") && !said.contains("resumes at step 3"),
            "rerun {}: {said}",
            n + 1
        );
        for (width, height) in [(240, 67), (160, 45)] {
            let sc = main_screen(width, height);
            let stage = stage_line(&fx.draw(&sc, fx.now).0, &sc);
            assert!(
                stage.contains("repairing"),
                "rerun {} at {width}x{height}: {stage:?}",
                n + 1
            );
        }
    }
}

fn header_row(buf: &Buffer, screen: &Screen) -> String {
    rows_of(buf)[usize::from(Layout::new(screen).header.y)]
        .trim()
        .to_owned()
}

/// The minutes in the whole `XhYYm` that follows the word `label` on the header row; a cut value has none.
fn header_minutes(row: &str, label: &str) -> Option<u64> {
    let mut words = row.split_whitespace();
    words.find(|&word| word == label)?;
    let (hours, minutes) = words.next()?.strip_suffix('m')?.split_once('h')?;
    if minutes.len() != 2 {
        return None;
    }
    Some(hours.parse::<u64>().ok()? * 60 + minutes.parse::<u64>().ok()?)
}

/// The tuning time and the session age of the header row, each in minutes and whether its label is the long one,
/// whichever label each wears.
fn header_clocks(row: &str) -> Option<((bool, u64), (bool, u64))> {
    let clock = |long, short| {
        header_minutes(row, long)
            .map(|minutes| (true, minutes))
            .or_else(|| header_minutes(row, short).map(|minutes| (false, minutes)))
    };
    Some((clock("tuning", "tune")?, clock("session", "age")?))
}

#[test]
fn the_header_shows_tuning_time_within_the_session_age_and_freezes_both_with_the_frame() {
    for scenario in [
        Scenario::Search,
        Scenario::Idle,
        Scenario::Stopped,
        Scenario::Deadend,
    ] {
        let fx = Fixture::scenario(scenario);
        for (width, height) in ALL_SIZES {
            let screen = main_screen(width, height);
            let (buf, _) = fx.draw(&screen, fx.now);
            let head = header_row(&buf, &screen);
            let ((tuning_long, tuned), (age_long, aged)) = header_clocks(&head).unwrap_or_else(|| {
                panic!("{scenario} {width}x{height} lacks a whole tuning time or session age: {head:?}")
            });
            assert!(
                width < 160 || (tuning_long && age_long),
                "{scenario} {width}x{height} abbreviates a clock it has room for: {head:?}"
            );
            assert!(tuned <= aged, "{scenario} {width}x{height}: {head:?}");
            if matches!(scenario, Scenario::Search) {
                continue;
            }
            // The wall clock and the failure's age move with the time; the two durations hold still while the
            // frame does. Their labels may differ when a failure's longer age takes room, so only the minutes
            // are compared.
            let (after, _) = fx.draw(&screen, later(fx.now, 7 * 3600));
            let after = header_row(&after, &screen);
            let held = header_clocks(&after).map(|((_, tuned), (_, aged))| (tuned, aged));
            assert_eq!(
                held,
                Some((tuned, aged)),
                "{scenario} {width}x{height}: a frozen frame's clocks moved: {after:?}"
            );
        }
    }
}
