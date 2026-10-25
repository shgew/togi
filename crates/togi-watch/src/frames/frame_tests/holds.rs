//! How long a frame stays true: an idle (R6) trial freezes every frame until a later event releases it, and every
//! other frame moves with the clock.

use jiff::tz::TimeZone;
use ratatui::style::Color;

use super::bands::{stage_line, summary_screen};
use super::{
    ALL_SIZES, SIZES, VIEWS, bar, clock, draw_journal, later, main_screen, one_frame, text, words,
};
use crate::frames::hand::Staged;
use crate::frames::testing::screen;
use crate::screen::{Colours, Hold, Journal, Screen, View};

#[test_case::test_case(true, false; "live keys")]
#[test_case::test_case(false, false; "live no keys")]
#[test_case::test_case(false, true; "one frame")]
fn an_idle_trial_freezes_every_view_until_its_end_is_recorded(keys: bool, one_frame: bool) {
    let fx = Staged::Idle.hand();
    let hold = fx.snapshot().idle_hold().expect("a started idle trial");
    let planned_end = clock(hold.planned_end, &TimeZone::UTC, "%H:%M");
    let first_at = later(hold.started, 1);
    let times = [
        hold.started,
        later(hold.planned_end, -1),
        later(hold.planned_end, 60),
        later(hold.planned_end, 3600),
        later(hold.planned_end, 86_400),
    ];
    for view in VIEWS {
        for (width, height) in ALL_SIZES {
            let sc = Screen {
                scroll: usize::MAX,
                one_frame,
                ..screen(view, width, height, Colours::Full, keys)
            };
            let (first, drawn) = fx.draw(&sc, first_at);
            assert_eq!(drawn.hold, Hold::Frozen, "{view:?} {width}x{height}");
            for at in times {
                let (later_frame, later_drawn) = fx.draw(&sc, at);
                assert_eq!(
                    later_frame, first,
                    "{view:?} {width}x{height} changes at {at} during the idle trial"
                );
                assert_eq!(later_drawn, drawn);
            }
            if view == View::Main {
                let shown = text(&first);
                for crash in ["crashed", "CRASHED", "togi run records the crash"] {
                    assert!(
                        !shown.contains(crash),
                        "an idle trial is narrated as crashed ({crash:?}):\n{shown}"
                    );
                }
                assert!(
                    !shown.contains("left"),
                    "an idle trial shows a changing countdown:\n{shown}"
                );
                assert!(
                    !shown.contains("past"),
                    "an idle trial is never past its end:\n{shown}"
                );
                assert!(
                    shown.contains(&format!("planned end {planned_end}")),
                    "the planned end is {planned_end}:\n{shown}"
                );
                assert!(
                    !shown.contains("paused until"),
                    "a planned end is not a release time:\n{shown}"
                );
            }
        }
    }
}

#[test]
fn an_idle_trial_shows_its_clock_at_the_trials_start_and_an_unfilled_bar() {
    let fx = Staged::Idle.hand();
    let hold = fx.snapshot().idle_hold().expect("a started idle trial");
    let started = clock(hold.started, &TimeZone::UTC, "%H:%M:%S");
    let planned_end = clock(hold.planned_end, &TimeZone::UTC, "%H:%M");
    for (width, height) in SIZES {
        for at in [fx.now, later(hold.planned_end, 7200)] {
            let (buf, _) = fx.draw(&main_screen(width, height), at);
            let shown = text(&buf);
            assert!(
                shown.contains(&format!("togi   {started}")),
                "{width}x{height} header clock is the trial's start:\n{shown}"
            );
            assert!(
                shown.contains(&format!("planned end {planned_end}")),
                "{width}x{height}:\n{shown}"
            );
            let bar = bar(&buf).expect("the trial bar");
            assert!(
                bar.chars().all(|c| c == '░') && bar.chars().count() > 20,
                "{width}x{height}: an idle bar has no elapsed fill: {bar:?}"
            );
        }
    }
}

#[test]
fn a_later_boot_releases_an_idle_trial_and_the_clock_moves_again() {
    let mut fx = Staged::Idle.hand();
    let hold = fx.snapshot().idle_hold().expect("a started idle trial");
    let planned_end = clock(hold.planned_end, &TimeZone::UTC, "%H:%M");
    fx.trial.as_mut().expect("an idle trial").crashed = true;
    let snapshot = fx.snapshot();
    assert!(snapshot.idle_hold().is_none());
    assert!(snapshot.crashed_trial());
    let id = snapshot.trial.expect("a released trial").id.clone();
    for (width, height) in ALL_SIZES {
        for view in VIEWS {
            let sc = Screen {
                scroll: usize::MAX,
                ..screen(view, width, height, Colours::Full, true)
            };
            let (now, drawn) = fx.draw(&sc, fx.now);
            assert_eq!(drawn.hold, Hold::Never, "{view:?} {width}x{height}");
            let (hour_later, _) = fx.draw(&sc, later(fx.now, 3600));
            assert_ne!(
                now, hour_later,
                "{view:?} {width}x{height}: the clock moves again"
            );
        }
        for one in [false, true] {
            let sc = Screen {
                one_frame: one,
                ..main_screen(width, height)
            };
            let said = words(&fx.draw(&sc, fx.now).0);
            let want_id = format!("trial {id}");
            for want in [
                "CRASHED, NOT YET RECOVERED",
                "togi run records the crash",
                &want_id,
            ] {
                assert!(
                    said.contains(want),
                    "{width}x{height} lacks {want:?}: {said}"
                );
            }
            for bad in [
                "left",
                "past its",
                "RECOVERED FROM A CRASH",
                &format!("planned end {planned_end}"),
            ] {
                assert!(!said.contains(bad), "{width}x{height} says {bad:?}: {said}");
            }
        }
    }
    let summary = summary_screen();
    let (wide, _) = fx.draw(&summary, fx.now);
    let stage = stage_line(&wide, &summary);
    assert!(
        !stage.contains('►') && !stage.contains("left") && stage.contains("trial crashed"),
        "{stage:?}"
    );
}

#[test]
fn a_trial_intent_before_its_start_does_not_freeze() {
    let mut fx = Staged::Idle.hand();
    fx.trial.as_mut().expect("an idle trial").started = None;
    let snapshot = fx.snapshot();
    assert!(snapshot.idle_hold().is_none());
    for (width, height) in ALL_SIZES {
        let (buf, drawn) = fx.main(width, height);
        assert_eq!(drawn.hold, Hold::Never, "{width}x{height}");
        let shown = text(&buf).to_lowercase();
        assert!(
            shown.contains("preparing") || shown.contains("waiting") || shown.contains("starting"),
            "{width}x{height}: a trial intent is presented as running:\n{shown}"
        );
        let (hour_later, _) = fx.draw(&main_screen(width, height), later(fx.now, 3600));
        assert_ne!(buf, hour_later, "{width}x{height}: the clock moves");
    }
}

#[test_case::test_case(Staged::Mapping; "mapping")]
#[test_case::test_case(Staged::Qualifying; "qualifying")]
#[test_case::test_case(Staged::Day; "day")]
#[test_case::test_case(Staged::Crashed; "crashed")]
#[test_case::test_case(Staged::Recovering; "recovering")]
#[test_case::test_case(Staged::Between; "between")]
#[test_case::test_case(Staged::Deadend; "deadend")]
#[test_case::test_case(Staged::Stopped; "stopped")]
fn every_other_frame_moves_with_the_clock(staged: Staged) {
    let fx = staged.hand();
    let name = staged.name();
    for (width, height) in SIZES {
        let (now, drawn) = fx.main(width, height);
        assert_eq!(drawn.hold, Hold::Never, "{name} {width}x{height}");
        let (later_frame, _) = fx.draw(&main_screen(width, height), later(fx.now, 61));
        assert_ne!(now, later_frame, "{name} {width}x{height} shows the clock");
    }
}

#[test_case::test_case(Staged::Mapping; "mapping")]
#[test_case::test_case(Staged::Qualifying; "qualifying")]
#[test_case::test_case(Staged::Reproducing; "reproducing")]
#[test_case::test_case(Staged::Day; "day")]
#[test_case::test_case(Staged::Idle; "idle")]
#[test_case::test_case(Staged::Crashed; "crashed")]
#[test_case::test_case(Staged::Between; "between")]
#[test_case::test_case(Staged::Deadend; "deadend")]
#[test_case::test_case(Staged::Stopped; "stopped")]
fn a_one_frame_print_with_no_colour_styles_no_cell(staged: Staged) {
    let fx = staged.hand();
    assert_unstyled(&fx.journal(), fx.now);
}

#[test_case::test_case(&Journal::Missing; "missing")]
#[test_case::test_case(&Journal::Starting; "starting")]
#[test_case::test_case(&Journal::Problem(&std::io::Error::other("read journal: permission denied")); "problem")]
fn an_unavailable_journal_print_with_no_colour_styles_no_cell(journal: &Journal<'_>) {
    let at = Staged::Mapping.hand().now;
    assert_unstyled(journal, at);
}

fn assert_unstyled(journal: &Journal<'_>, at: jiff::Timestamp) {
    for (width, height) in SIZES {
        for view in VIEWS {
            let sc = Screen {
                view,
                ..one_frame(width, height)
            };
            let (buf, _) = draw_journal(journal, &sc, at, &TimeZone::UTC);
            for (x, y, cell) in buf.content.iter().enumerate().map(|(i, cell)| {
                let width = usize::from(buf.area.width);
                (i % width, i / width, cell)
            }) {
                assert!(
                    cell.fg == Color::Reset && cell.bg == Color::Reset && cell.modifier.is_empty(),
                    "{journal:?} {view:?} {width}x{height}: cell ({x}, {y}) {:?} is styled",
                    cell.symbol()
                );
            }
        }
    }
}
