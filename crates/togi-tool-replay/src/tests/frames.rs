use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
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

fn shown_snapshot(snapshot: &str, body: &str) {
    let expected = snapshot.split_once("\n---\n").unwrap().1;
    assert_eq!(body.trim_end(), expected.trim_end());
}

#[test]
fn the_main_view_of_a_prefix_is_printed_as_plain_text() {
    let events = scenario(Scenario::Checking);
    let body = text(&events, events.len(), &frame(View::Main, (120, 33)));
    assert!(!body.contains('\x1b'));
    shown_snapshot(include_str!("snapshots/main-checking-120x33.snap"), &body);
}

#[test]
fn the_help_and_log_views_are_printed_with_their_page_names_and_no_key_hints() {
    let events = scenario(Scenario::Checking);
    let help = text(&events, events.len(), &frame(View::Help, (80, 24)));
    let log = text(&events, events.len(), &frame(View::Log, (80, 24)));
    for (print, place) in [(&help, "9/9 Help"), (&log, "8/9 Journal")] {
        let footer = print.lines().last().unwrap_or_default();
        assert!(footer.contains(place), "{footer:?}");
        assert!(
            !footer.contains("Esc") && !footer.contains("PgUp"),
            "{footer:?}"
        );
    }
    shown_snapshot(include_str!("snapshots/help-80x24.snap"), &help);
    shown_snapshot(include_str!("snapshots/log-80x24.snap"), &log);
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
    assert!(togi_machine::error_chain(&error).starts_with("event 2 cannot be shown: "));
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

fn written(colour: bool, rows: &[&[(&str, Style)]]) -> String {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 12, u16::try_from(rows.len()).unwrap()));
    for (y, row) in (0..).zip(rows) {
        let mut x = 0;
        for &(piece, style) in *row {
            buffer.set_string(x, y, piece, style);
            x += u16::try_from(piece.chars().count()).unwrap();
        }
    }
    crate::frame::text(&buffer, colour)
}

#[test]
fn text_without_colour_trims_each_row_and_keeps_wide_glyphs_whole() {
    let red = Style::new().fg(Color::Red).bg(Color::Blue);
    let body = written(
        false,
        &[
            &[("a b", Style::default()), ("   ", red)],
            &[("漢字x", Style::default())],
            &[],
        ],
    );
    assert_eq!(body, "a b\n漢字x\n\n");
}

#[test]
fn colour_writes_one_sequence_per_change_of_style_and_resets_at_the_end_of_a_row() {
    let plain = Style::default();
    let bold_red = Style::new()
        .fg(Color::Red)
        .bg(Color::Blue)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let all = Style::new().add_modifier(
        Modifier::DIM | Modifier::ITALIC | Modifier::REVERSED | Modifier::CROSSED_OUT,
    );
    let body = written(
        true,
        &[
            &[("a", plain), ("b", bold_red), ("c", bold_red), ("d", plain)],
            &[("e", all)],
            &[(
                "f",
                Style::new().fg(Color::Indexed(200)).bg(Color::Rgb(1, 2, 3)),
            )],
            &[
                ("g", Style::new().bg(Color::Green)),
                (" ", Style::new().bg(Color::Green)),
            ],
        ],
    );
    assert_eq!(
        body,
        "a\x1b[0;1;4;31;44mbc\x1b[0md\n\
         \x1b[0;2;3;7;9me\x1b[0m\n\
         \x1b[0;38;5;200;48;2;1;2;3mf\x1b[0m\n\
         \x1b[0;42mg \x1b[0m\n"
    );
}

#[test]
fn every_colour_slot_has_its_own_code_as_a_foreground_and_a_background() {
    let slots = [
        (Color::Black, 0),
        (Color::Red, 1),
        (Color::Green, 2),
        (Color::Yellow, 3),
        (Color::Blue, 4),
        (Color::Magenta, 5),
        (Color::Cyan, 6),
        (Color::Gray, 7),
        (Color::DarkGray, 8),
        (Color::LightRed, 9),
        (Color::LightGreen, 10),
        (Color::LightYellow, 11),
        (Color::LightBlue, 12),
        (Color::LightMagenta, 13),
        (Color::LightCyan, 14),
        (Color::White, 15),
    ];
    for (color, slot) in slots {
        let (fg, bg) = if slot < 8 {
            (30 + slot, 40 + slot)
        } else {
            (82 + slot, 92 + slot)
        };
        let body = written(true, &[&[("x", Style::new().fg(color).bg(color))]]);
        assert_eq!(body, format!("\x1b[0;{fg};{bg}mx\x1b[0m\n"), "{color:?}");
    }
}

#[test]
fn a_started_idle_trial_is_drawn_the_same_however_long_after_the_event() {
    let events = scenario(Scenario::Idle);
    let at = |after| {
        let frame = Frame {
            after: SignedDuration::from_secs(after),
            ..frame(View::Main, (240, 67))
        };
        text(&events, events.len(), &frame)
    };
    assert_eq!(at(0), at(3 * 3600));
}

#[test]
fn a_running_trial_shows_the_time_that_has_passed() {
    let events = scenario(Scenario::Checking);
    let at = |after| {
        let frame = Frame {
            after: SignedDuration::from_secs(after),
            ..frame(View::Main, (240, 67))
        };
        text(&events, events.len(), &frame)
    };
    assert_ne!(at(1), at(89));
}
