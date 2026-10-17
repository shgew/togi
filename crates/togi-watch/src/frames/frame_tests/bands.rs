//! What the frame says about the session's state: the story, the NOW band and the stage line, whole.

use ratatui::buffer::Buffer;
use togi_journal::{
    CommandReset, DeadEnd, DeadEndCondition, EventKind, Outcome, ProfileRestored, ResetTarget,
    Shutdown, ShutdownReason, TrialProgress,
};
use togi_machine::Signal;
use togi_watchtest::{Scenario, cut_at};

use super::{
    ALL_SIZES, Fixture, SIZES, append, later, main_screen, one_frame, row_of, row_starting, text,
    words,
};
use crate::frames::layout::Layout;
use crate::frames::testing::rows_of;
use crate::screen::{Hold, Screen, View};
use crate::tests::kit::{Script, crash, end_payload, offsets, warning};

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
fn a_stopped_frame_labels_saved_offsets_as_not_applied_and_shows_no_running_stage() {
    let fx = Fixture::scenario(Scenario::Stopped);
    assert!(fx.model.snapshot().stopped.is_some_and(|stop| stop.saved));
    for (width, height) in ALL_SIZES {
        let (buf, _) = fx.main(width, height);
        let shown = text(&buf).to_lowercase();
        assert!(
            shown.contains("stopped")
                && shown.contains("saved")
                && (shown.contains("not applied") || shown.contains("not set")),
            "{width}x{height} misrepresents restored hardware:\n{shown}"
        );
        let stage = stage_line(&buf, &main_screen(width, height));
        assert!(
            !stage.contains('►') && !stage.contains("repeats until stopped"),
            "{width}x{height}: a stopped stage line shows a running stage: {stage:?}"
        );
    }
    let (wide, _) = fx.main(240, 67);
    assert!(
        words(&wide).contains("The requested clean cycles are complete"),
        "the story says why the session stopped"
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

#[test]
fn a_resting_band_of_one_line_leaves_the_ccd_table_its_rows() {
    let mut script = Script::new(&[(0, 0)], &[crate::tests::kit::Start::search(0)]);
    script.append(
        Shutdown {
            reason: ShutdownReason::Signal,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }
        .into(),
        &[],
    );
    let fx = Fixture::of(&script.events);
    for (width, height, gap) in [(240, 67, 3), (160, 45, 3), (120, 33, 2)] {
        let (buf, _) = fx.main(width, height);
        let band = row_starting(&buf, "STOPPED").expect("the resting band");
        let ccd = row_of(&buf, "CCD 0").expect("the CCD table");
        assert_eq!(
            ccd - band,
            gap,
            "{width}x{height}: a one-line band at row {band} leaves the CCD table at row {ccd}\n{}",
            text(&buf)
        );
    }
}

#[test]
fn a_dead_end_frame_keeps_the_recorded_cause_and_invents_no_diagnosis() {
    for (condition, detail, wrong) in [
        (
            DeadEndCondition::NoEvidence,
            "kernel log unreadable: permission denied",
            "installed",
        ),
        (
            DeadEndCondition::Containment,
            "termination unconfirmed",
            "termination confirmed",
        ),
        (
            DeadEndCondition::ThermalTrip,
            "firmware reported a thermal trip",
            "wrong result",
        ),
    ] {
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
}

#[test]
fn a_restored_dead_end_labels_its_rows_as_saved() {
    let mut script = Script::at_limit();
    script.append(
        DeadEnd {
            condition: DeadEndCondition::NoEvidence,
            core: None,
            detail: "five trials in a row proved nothing".to_owned(),
            action: None,
        }
        .into(),
        &[],
    );
    script.append(
        ProfileRestored {
            offsets: offsets(&[0, 0, 0]),
        }
        .into(),
        &[],
    );
    script.append(
        Shutdown {
            reason: ShutdownReason::DeadEnd,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }
        .into(),
        &[],
    );
    let fx = Fixture::of(&script.events);
    let snapshot = fx.model.snapshot();
    assert!(snapshot.dead_end.is_some() && snapshot.stopped.is_some_and(|stop| stop.saved));
    for (width, height) in SIZES {
        let said = words(&fx.main(width, height).0);
        assert!(
            said.contains("Rows show the saved profile, not applied now."),
            "{width}x{height} presents saved offsets as applied: {said}"
        );
    }
}

#[test]
fn no_trial_yet_does_not_claim_one_ended() {
    let events = session_events();
    let first_intent = events
        .iter()
        .position(|event| event.kind() == EventKind::TrialIntent)
        .expect("a trial intent");
    let ended = cut_at(&events, |event| event.kind() == EventKind::TrialEnd).expect("a trial end");
    for (name, cut, want) in [
        (
            "session start to readback",
            &events[..first_intent],
            "No trial has run yet",
        ),
        ("first trial ended", ended, "The last trial has ended"),
    ] {
        let fx = Fixture::of(cut);
        assert!(fx.model.snapshot().trial.is_none(), "{name}");
        for (width, height) in SIZES {
            let said = words(&fx.main(width, height).0);
            assert!(
                said.contains(want),
                "{name} at {width}x{height} lacks {want:?}: {said}"
            );
            if want == "No trial has run yet" {
                assert!(
                    !said.contains("has ended"),
                    "{name} at {width}x{height} claims a trial ended: {said}"
                );
            }
        }
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
            "crashed",
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
        assert!(
            before.contains("left") && !before.contains("past"),
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
            for bad in ["STOPPED", "left", "past its end", "RECOVERED FROM A CRASH"] {
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
        !stage.contains('►') && !stage.contains("left") && stage.contains("trial crashed"),
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
        let said = words(&wide);
        assert!(
            said.contains("Cycle 1 waits at step 2 until the rerun passes."),
            "rerun {}: {said}",
            n + 1
        );
        assert!(
            !said.contains("waits at step 1 "),
            "rerun {}: {said}",
            n + 1
        );
        for (width, height) in [(240, 67), (160, 45)] {
            let sc = main_screen(width, height);
            let stage = stage_line(&fx.draw(&sc, fx.now).0, &sc);
            assert!(
                stage.contains("paused at step 2 of"),
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

/// The minutes in the `XhYYm` that follows `label` on the header row.
fn header_minutes(row: &str, label: &str) -> Option<u64> {
    let value = row.split_once(label)?.1.split_whitespace().next()?;
    let (hours, minutes) = value.split_once('h')?;
    Some(hours.parse::<u64>().ok()? * 60 + minutes.trim_end_matches('m').parse::<u64>().ok()?)
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
            let long = head.contains("tuning ") && head.contains("session ");
            let short = head.contains("tune ") && head.contains("age ");
            assert!(
                long || short,
                "{scenario} {width}x{height} lacks a clock: {head:?}"
            );
            assert!(
                width < 160 || long,
                "{scenario} {width}x{height} abbreviates a clock it has room for: {head:?}"
            );
            let (tuning, age) = if long {
                ("tuning ", "session ")
            } else {
                ("tune ", "age ")
            };
            let (tuned, aged) = (
                header_minutes(&head, tuning).expect("the tuning time"),
                header_minutes(&head, age).expect("the session age"),
            );
            assert!(tuned <= aged, "{scenario} {width}x{height}: {head:?}");
            if matches!(scenario, Scenario::Search) {
                continue;
            }
            // The wall clock and the failure's age move with the time; the two durations hold still while the
            // frame does.
            let (after, _) = fx.draw(&screen, later(fx.now, 7 * 3600));
            let after = header_row(&after, &screen);
            assert_eq!(
                (header_minutes(&after, tuning), header_minutes(&after, age)),
                (Some(tuned), Some(aged)),
                "{scenario} {width}x{height}: a frozen frame's clocks moved: {after:?}"
            );
        }
    }
}
