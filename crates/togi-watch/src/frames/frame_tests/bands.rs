//! What the frame says about the session's state: the story, the NOW band and the stage line, whole.

use ratatui::buffer::Buffer;
use test_case::test_case;
use togi_journal::DeadEndCondition;

use super::{ALL_SIZES, SIZES, later, main_screen, one_frame, text, words};
use crate::frames::hand::{Hand, Staged, ended_trial};
use crate::frames::layout::Layout;
use crate::frames::testing::{Fixture, rows_of};
use crate::model::{DeadEndView, RecoveryView, StopView};
use crate::screen::{Hold, Screen, View};
use togi_journal::ShutdownReason;
use togi_machine::Regime;
use togi_watchtest::Scenario;

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

#[test]
fn between_trials_names_the_last_outcome_and_not_a_countdown() {
    let fx = Staged::Between.hand();
    assert!(fx.snapshot().trial.is_none() && fx.snapshot().last.is_some());
    for (width, height) in SIZES {
        let (buf, drawn) = fx.main(width, height);
        let shown = text(&buf);
        assert!(
            shown.contains("BETWEEN TRIALS"),
            "{width}x{height} loses its state:\n{shown}"
        );
        assert!(
            shown.lines().all(|row| !row.trim_end().ends_with(" left")),
            "{width}x{height} invents a countdown:\n{shown}"
        );
        assert_eq!(drawn.hold, Hold::Never);
    }
}

#[test]
fn a_stopped_frame_shows_the_saved_profile_and_no_running_stage() {
    let fx = Staged::Stopped.hand();
    assert!(fx.snapshot().stopped.is_some_and(|stop| stop.saved));
    for (width, height) in ALL_SIZES {
        let (buf, _) = fx.main(width, height);
        let shown = text(&buf).to_lowercase();
        assert!(
            shown.contains("stopped") && shown.contains("saved"),
            "{width}x{height} hides the saved profile:\n{shown}"
        );
        let stage = stage_line(&buf, &main_screen(width, height));
        assert!(
            stage.starts_with("○ STOPPED") && !stage.contains('►'),
            "{width}x{height}: a stopped stage line shows a running stage: {stage:?}"
        );
    }
}

#[test]
fn a_running_stage_line_keeps_its_marker_and_names_the_night() {
    let fx = Staged::Mapping.hand();
    let (buf, _) = fx.main(240, 67);
    let stage = stage_line(&buf, &main_screen(240, 67));
    assert!(
        stage.starts_with("► NIGHT since") && stage.contains("mapping CCD 1 at -20"),
        "{stage:?}"
    );
    let day = Staged::Day.hand();
    let (buf, _) = day.main(240, 67);
    assert!(
        stage_line(&buf, &main_screen(240, 67)).starts_with("► DAY"),
        "the day has its own stage"
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
    let mut fx = Staged::Deadend.hand();
    fx.dead_end = Some(DeadEndView {
        at: crate::frames::testing::FIXED_NOW,
        condition,
        detail: detail.to_owned(),
    });
    let (buf, _) = fx.main(240, 67);
    let shown = text(&buf);
    assert!(
        shown.contains("DEAD END") && shown.contains(detail) && !shown.contains(wrong),
        "a dead end loses its recorded cause or invents one:\n{shown}"
    );
}

#[test]
fn a_session_with_no_trial_yet_does_not_claim_one_ended() {
    let mut fx = Staged::Between.hand();
    fx.last = None;
    for (width, height) in SIZES {
        let said = words(&fx.main(width, height).0);
        assert!(
            said.contains("BETWEEN TRIALS") && !said.contains("last:"),
            "{width}x{height} names an outcome where none exists: {said}"
        );
        assert!(
            !said.contains("has ended"),
            "{width}x{height} claims a trial ended: {said}"
        );
    }
}

#[test]
fn a_first_trial_that_ended_is_reported_as_the_last_one() {
    let fx = Staged::Between.hand();
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
    let fx = Staged::Crashed.hand();
    let id = fx.snapshot().trial.expect("a crashed trial").id.clone();
    assert!(fx.snapshot().crashed_trial());
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
        for bad in ["left", "past its end"] {
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
    assert!(
        !Staged::Mapping.hand().snapshot().crashed_trial(),
        "a trial in the latest boot is running"
    );
}

#[test_case(Scenario::Mapping, 240, 67; "mapping wide")]
#[test_case(Scenario::Mapping, 160, 45; "mapping medium")]
#[test_case(Scenario::Mapping, 120, 33; "mapping compact")]
#[test_case(Scenario::Controls, 160, 45; "controls")]
#[test_case(Scenario::Filler, 160, 45; "filler")]
fn event_redraws_keep_every_counter_on_the_same_second(
    scenario: Scenario,
    width: u16,
    height: u16,
) {
    let mut events = scenario.events().expect("a running trial");
    for event in &mut events {
        event.time += jiff::SignedDuration::from_millis(375);
    }
    let fx = Fixture::of(&events);
    let trial = fx.model.snapshot().trial.expect("a started trial");
    let started = trial.started.expect("started");
    let second = started - jiff::SignedDuration::from_nanos(i64::from(started.subsec_nanosecond()));
    let screen = main_screen(width, height);
    for elapsed in 1..=8 {
        let at = later(second, elapsed);
        let (boundary, _) = fx.draw(&screen, at);
        let said = words(&boundary);
        let shown = |seconds: u64| format!("{}:{:02}", seconds / 60, seconds % 60);
        let remaining = trial.duration.as_secs() - elapsed.cast_unsigned();
        if width == 240 {
            assert!(
                said.contains(&format!(
                    "{} of {}",
                    shown(elapsed.cast_unsigned() - 1),
                    shown(trial.duration.as_secs())
                )),
                "elapsed second {elapsed}: {said}"
            );
        } else {
            assert!(
                said.contains(&format!("{} left", shown(remaining))),
                "second {elapsed}: {said}"
            );
        }
        for offset in (1..1000).step_by(137) {
            let event_at = at + jiff::SignedDuration::from_millis(offset);
            assert_eq!(
                fx.draw(&screen, event_at).0,
                boundary,
                "an event at second {elapsed} + {offset}ms must not advance any counter"
            );
        }
    }
}

#[test]
fn a_trial_past_its_planned_end_shows_how_long_without_claiming_a_crash() {
    let fx = Staged::Mapping.hand();
    let trial = fx.snapshot().trial.expect("a started trial").clone();
    let end = later(
        trial.started.expect("a started trial"),
        i64::try_from(trial.duration.as_secs()).expect("seconds"),
    );
    for (width, height) in SIZES {
        let sc = main_screen(width, height);
        let before = words(&fx.draw(&sc, later(end, -1)).0);
        let required = trial.duration.as_secs();
        let clock = |secs: u64| format!("{}:{:02}", secs / 60, secs % 60);
        let timed = if width >= 200 && height >= 60 {
            before.contains(&format!("{} of {}", clock(required - 1), clock(required)))
        } else {
            before.contains("0:01 left")
        };
        assert!(
            timed && !before.contains("past"),
            "{width}x{height} before the planned end: {before}"
        );
        let (buf, drawn) = fx.draw(&sc, later(end, 42));
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
        for bad in ["0:00 left", "crashed", "togi run records", "CRASHED"] {
            assert!(
                !said.contains(bad),
                "{width}x{height} says {bad:?} past the planned end: {said}"
            );
        }
        assert_eq!(drawn.hold, Hold::Never, "{width}x{height} keeps ticking");
        let (one, _) = fx.draw(&one_frame(width, height), later(end, 42));
        assert!(words(&one).contains("no trial end recorded"));
    }
    let summary = summary_screen();
    let (wide, _) = fx.draw(&summary, later(end, 42));
    assert!(
        stage_line(&wide, &summary).contains("0:42 past its end"),
        "the stage line says how long past its end"
    );
}

/// A crashed trial whose session also shows `mask`: the crash narration wins over a stale recovery or a stop.
fn crash_with(mask: impl FnOnce(&mut Hand)) -> Hand {
    let mut fx = Staged::Crashed.hand();
    mask(&mut fx);
    fx
}

#[test_case(
    |fx: &mut Hand| fx.recover = Some(RecoveryView {
        crash_at: crate::frames::testing::FIXED_NOW,
        reset: None,
        trial: None,
        end: Some(ended_trial(Regime::R1)),
    });
    "a prior recovery"
)]
#[test_case(
    |fx: &mut Hand| fx.stopped = Some(StopView {
        at: crate::frames::testing::FIXED_NOW,
        reason: ShutdownReason::Command,
        saved: true,
    });
    "a command shutdown"
)]
fn an_unrecovered_crash_is_not_masked(mask: fn(&mut Hand)) {
    let fx = crash_with(mask);
    let id = fx.snapshot().trial.expect("a crashed trial").id.clone();
    for (width, height) in SIZES {
        for sc in [main_screen(width, height), one_frame(width, height)] {
            let said = words(&fx.draw(&sc, fx.now).0);
            for want in [
                "CRASHED, NOT YET RECOVERED",
                &format!("trial {id}"),
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
        stage.contains("trial crashed") && !stage.contains('►') && !stage.contains("left"),
        "a masked crash presents a running stage: {stage:?}"
    );
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

#[test_case(Staged::Mapping, false; "mapping, whose clocks run")]
#[test_case(Staged::Idle, true; "idle")]
#[test_case(Staged::Stopped, true; "stopped")]
#[test_case(Staged::Deadend, true; "dead end")]
fn the_header_shows_tuning_time_within_the_session_age_and_freezes_both_with_the_frame(
    staged: Staged,
    frozen: bool,
) {
    let scenario = staged.name();
    let fx = staged.hand();
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
        if !frozen {
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
