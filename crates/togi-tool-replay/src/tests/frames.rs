use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use togi_journal::Event;
use togi_watch::screen::View;
use togi_watchtest::Scenario;

use crate::{Error, Frame, draw, offset, prefix};

fn frame(view: View, (width, height): (u16, u16)) -> Frame {
    Frame {
        view,
        width,
        height,
        colour: false,
        after: SignedDuration::ZERO,
    }
}

fn scenario(scenario: Scenario) -> Vec<Event> {
    scenario.events().unwrap()
}

fn text(events: &[Event], shown: usize, frame: &Frame) -> String {
    draw(events, shown, frame, &TimeZone::UTC).unwrap()
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn without_trailing_blanks(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn clock(time: Timestamp) -> String {
    time.to_zoned(TimeZone::UTC)
        .strftime("%H:%M:%S")
        .to_string()
}

fn shown_snapshot(name: &str, body: &str) {
    insta::with_settings!({ prepend_module_to_snapshot => false }, {
        insta::assert_snapshot!(name, body);
    });
}

#[test]
fn the_main_view_of_a_prefix_is_printed_as_plain_text() {
    let events = scenario(Scenario::Checking);
    let body = text(&events, events.len(), &frame(View::Main, (120, 33)));
    assert!(!body.contains('\x1b'));
    shown_snapshot("main-checking-120x33", &body);
}

#[test]
fn the_help_and_log_views_are_printed_without_the_key_footer() {
    let events = scenario(Scenario::Checking);
    shown_snapshot(
        "help-80x24",
        &text(&events, events.len(), &frame(View::Help, (80, 24))),
    );
    shown_snapshot(
        "log-80x24",
        &text(&events, events.len(), &frame(View::Log, (80, 24))),
    );
}

/// The newest short message the log lists as of the first `shown` events.
fn logged(events: &[Event], shown: usize) -> String {
    let model = togi_watch::model::project(&events[..shown]);
    model
        .snapshot()
        .log()
        .entries
        .iter()
        .rev()
        .find(|entry| (20..100).contains(&entry.msg.len()))
        .unwrap()
        .msg
        .clone()
}

#[test]
fn a_frame_is_the_state_as_of_the_event_not_the_end_of_the_recording() {
    let events = scenario(Scenario::Checking);
    let early = events.len() / 2;
    let wide = frame(View::Log, (240, 67));
    let at_early = text(&events, early, &wide);
    let at_end = text(&events, events.len(), &wide);
    assert!(at_early.contains(&logged(&events, early)));
    let newest = logged(&events, events.len());
    assert!(at_end.contains(&newest));
    assert!(
        !at_early.contains(&newest),
        "the earlier frame already shows {newest:?}"
    );
    assert_ne!(
        text(&events, early, &frame(View::Main, (240, 67))),
        text(&events, events.len(), &frame(View::Main, (240, 67)))
    );
}

#[test]
fn the_log_view_follows_the_end_of_the_prefix() {
    let events = scenario(Scenario::Checking);
    for shown in [events.len() / 3, events.len()] {
        let log = text(&events, shown, &frame(View::Log, (240, 67)));
        assert!(log.contains(&logged(&events, shown)), "{shown}");
    }
}

#[test]
fn the_frame_is_drawn_at_the_last_events_wall_time_plus_after() {
    let events = scenario(Scenario::Checking);
    let shown = events.len() / 2;
    let last = events[shown - 1].time;
    for after in [SignedDuration::ZERO, SignedDuration::from_secs(40)] {
        let main = Frame {
            after,
            ..frame(View::Main, (240, 67))
        };
        let header = text(&events, shown, &main);
        let header = header.lines().next().unwrap();
        assert!(
            header.contains(&clock(offset(last, after))),
            "{header:?} should show {}",
            clock(offset(last, after))
        );
    }
}

#[test]
fn colour_keeps_ansi_that_strips_back_to_the_plain_frame() {
    let events = scenario(Scenario::Checking);
    let plain = text(&events, events.len(), &frame(View::Main, (240, 67)));
    let coloured = text(
        &events,
        events.len(),
        &Frame {
            colour: true,
            ..frame(View::Main, (240, 67))
        },
    );
    assert!(coloured.contains("\x1b["));
    assert_eq!(
        without_trailing_blanks(&strip_ansi(&coloured)),
        without_trailing_blanks(&plain)
    );
    for line in coloured.lines().filter(|line| line.contains('\x1b')) {
        assert!(line.ends_with("\x1b[0m"), "{line:?} leaves its styling on");
    }
}

#[test]
fn a_frame_stays_inside_the_screen_at_any_size() {
    let events = scenario(Scenario::Checking);
    for size in [
        (240, 67),
        (120, 33),
        (80, 24),
        (40, 10),
        (10, 3),
        (2, 2),
        (1, 1),
    ] {
        for view in [View::Main, View::Help, View::Log] {
            let body = text(&events, events.len(), &frame(view, size));
            let rows = body.lines().count();
            assert_eq!(rows, usize::from(size.1 - 1), "{view:?} at {size:?}");
            let widest = body
                .lines()
                .map(|line| line.chars().count())
                .max()
                .unwrap_or(0);
            assert!(
                widest < usize::from(size.0).max(2),
                "{view:?} at {size:?}: {widest}"
            );
        }
    }
}

#[test]
fn a_prefix_is_held_inside_the_recording() {
    let events = scenario(Scenario::Checking);
    let main = frame(View::Main, (120, 33));
    assert_eq!(text(&events, 0, &main), text(&events, 1, &main));
    assert_eq!(
        text(&events, events.len() + 10, &main),
        text(&events, events.len(), &main)
    );
    assert_eq!(prefix(&events, 0), 1);
}

#[test]
fn a_recording_the_dashboard_cannot_fold_names_the_event_that_breaks_it() {
    let mut events = scenario(Scenario::Checking);
    let mut second_start = events[0].clone();
    second_start.seq = events[1].seq;
    events.truncate(1);
    events.push(second_start);
    let error = draw(&events, 2, &frame(View::Main, (80, 24)), &TimeZone::UTC).unwrap_err();
    assert!(matches!(error, Error::Fold { seq: 2, .. }), "{error}");
    assert!(error.to_string().starts_with("event 2 cannot be shown: "));
}

#[test]
fn every_scenario_can_be_replayed_to_its_last_event() {
    for scenario in Scenario::ALL {
        let events = scenario.events().unwrap();
        let body = text(&events, events.len(), &frame(View::Main, (240, 67)));
        assert!(
            body.lines().any(|line| !line.is_empty()),
            "{}",
            scenario.name()
        );
    }
}
