//! How long a frame stays true: an idle (R6) trial freezes every frame until a later event releases it, and every
//! other frame moves with the clock.

use jiff::tz::TimeZone;
use ratatui::style::Color;
use togi_journal::{EventKind, Payload};
use togi_machine::Regime;
use togi_watchtest::{Scenario, cut_at};

use super::bands::{stage_line, summary_screen};
use super::{
    ALL_SIZES, Fixture, SIZES, VIEWS, append, clock, draw_journal, later, main_screen, one_frame,
    rows_of, text, words,
};
use crate::frames::measure::{Main, MainForm};
use crate::frames::testing::screen;
use crate::screen::{Colours, Hold, Journal, Screen, View};

#[test]
fn an_idle_trial_freezes_every_view_until_its_end_is_recorded() {
    let fx = Fixture::scenario(Scenario::Idle);
    let hold = fx
        .model
        .snapshot()
        .idle_hold()
        .expect("a started idle trial");
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
            for (keys, one_frame) in [(true, false), (false, false), (false, true)] {
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
                if view == View::Main && Main::new(&sc).form != MainForm::Scrolled {
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
}

#[test]
fn an_idle_trial_shows_its_clock_at_the_trials_start_and_its_planned_end_without_a_bar() {
    let fx = Fixture::scenario(Scenario::Idle);
    let hold = fx
        .model
        .snapshot()
        .idle_hold()
        .expect("a started idle trial");
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
            let trial =
                &rows_of(&buf)[usize::from(Main::new(&main_screen(width, height)).now.y) + 1];
            assert!(
                trial.contains("planned end") && trial.contains('░') && !trial.contains('█'),
                "{width}x{height}: an idle trial shows its planned end and an unfilled bar: {trial:?}"
            );
        }
    }
}

#[test]
fn a_later_boot_releases_an_idle_trial_and_the_clock_moves_again() {
    let mut events = Scenario::Idle.events().expect("the scenario's journal");
    let hold = Fixture::of(&events)
        .model
        .snapshot()
        .idle_hold()
        .expect("a started idle trial");
    let config = events
        .iter()
        .find(|event| event.kind() == EventKind::ConfigLoaded)
        .expect("the configuration recorded before the trial")
        .payload
        .clone();
    let planned_end = clock(hold.planned_end, &TimeZone::UTC, "%H:%M");
    append(&mut events, config, 3600, "after-idle");
    let fx = Fixture::of(&events);
    let snapshot = fx.model.snapshot();
    assert!(snapshot.idle_hold().is_none());
    assert!(snapshot.crashed_trial());
    let id = snapshot
        .trial
        .as_ref()
        .expect("a released trial")
        .id
        .clone();
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
    let events = Scenario::Idle.events().expect("the scenario's journal");
    let before_start = Scenario::Idle
        .base()
        .events()
        .and_then(|base| {
            cut_at(base, |event| {
                matches!(&event.payload, Payload::TrialIntent(intent) if intent.regime == Regime::R6)
            })
            .map(<[_]>::to_vec)
        })
        .expect("the idle trial's intent");
    assert!(before_start.len() < events.len());
    let fx = Fixture::of(&before_start);
    let snapshot = fx.model.snapshot();
    assert!(snapshot.trial.is_some_and(|trial| trial.started.is_none()));
    assert!(snapshot.idle_hold().is_none());
    for (width, height) in ALL_SIZES {
        let (buf, drawn) = fx.main(width, height);
        assert_eq!(drawn.hold, Hold::Never, "{width}x{height}");
        let shown = text(&buf).to_lowercase();
        assert!(
            shown.contains("preparing") || shown.contains("waiting") || shown.contains("starting"),
            "{width}x{height}: a trial intent is presented as running:\n{shown}"
        );
        assert!(
            !shown.contains("offsets are set"),
            "{width}x{height}: an intent before its readback claims the offsets are set:\n{shown}"
        );
        let (hour_later, _) = fx.draw(&main_screen(width, height), later(fx.now, 3600));
        assert_ne!(buf, hour_later, "{width}x{height}: the clock moves");
    }
}

#[test]
fn every_other_frame_moves_with_the_clock() {
    for scenario in [
        Scenario::Search,
        Scenario::Checking,
        Scenario::Crashed,
        Scenario::Recovering,
        Scenario::Between,
        Scenario::Deadend,
        Scenario::Stopped,
    ] {
        let fx = Fixture::scenario(scenario);
        for (width, height) in SIZES {
            let (now, drawn) = fx.main(width, height);
            assert_eq!(drawn.hold, Hold::Never, "{scenario} {width}x{height}");
            let (later_frame, _) = fx.draw(&main_screen(width, height), later(fx.now, 61));
            assert_ne!(
                now, later_frame,
                "{scenario} {width}x{height} shows the clock"
            );
        }
    }
}

#[test]
fn a_one_frame_print_with_no_colour_styles_no_cell() {
    let error = std::io::Error::other("read journal: permission denied");
    let sessions: Vec<Fixture> = [
        Scenario::Search,
        Scenario::Checking,
        Scenario::Hunt,
        Scenario::Idle,
        Scenario::Crashed,
        Scenario::Between,
        Scenario::Deadend,
        Scenario::Stopped,
    ]
    .map(Fixture::scenario)
    .into();
    let mut journals: Vec<Journal<'_>> = sessions.iter().map(Fixture::journal).collect();
    journals.extend([
        Journal::Missing,
        Journal::Starting,
        Journal::Problem(&error),
    ]);
    for journal in journals {
        for (width, height) in SIZES {
            for view in VIEWS {
                let sc = Screen {
                    view,
                    ..one_frame(width, height)
                };
                let at = sessions[0].now;
                let (buf, _) = draw_journal(&journal, &sc, at, &TimeZone::UTC);
                for (x, y, cell) in buf.content.iter().enumerate().map(|(i, cell)| {
                    let width = usize::from(buf.area.width);
                    (i % width, i / width, cell)
                }) {
                    assert!(
                        cell.fg == Color::Reset
                            && cell.bg == Color::Reset
                            && cell.modifier.is_empty(),
                        "{journal:?} {view:?} {width}x{height}: cell ({x}, {y}) {:?} is styled",
                        cell.symbol()
                    );
                }
            }
        }
    }
}
