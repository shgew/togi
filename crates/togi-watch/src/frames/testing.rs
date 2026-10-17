//! Test support for every frame test: building a frame buffer, checking its bounds and its glyphs, and
//! rendering it as text for snapshots.

use std::fmt::Write as _;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Style};
use togi_watchtest::Scenario;

use super::colours::Paint;
use super::fit::is_console_glyph;
use super::layout::{check_frame_bounds, frame_area};
use super::render::render;
use crate::model::{Model, project};
use crate::screen::{Colours, Drawn, Journal, Quit, Screen, View};

/// A moment for frames whose journal has no events to take a clock from.
pub(crate) const FIXED_NOW: Timestamp = Timestamp::constant(1_700_000_000, 0);

/// The folded journal of `scenario`, and the time the dashboard draws it at: 40 seconds after its last event.
pub(crate) fn scenario_model(scenario: Scenario) -> (Model, Timestamp) {
    let events = scenario
        .events()
        .unwrap_or_else(|error| panic!("{}: {error}", scenario.name()));
    let last = events.last().expect("a scenario has events").time;
    (project(&events), last + SignedDuration::from_secs(40))
}

/// `model` as the ready journal the frames draw from, with its own log list.
pub(crate) fn ready(model: &Model) -> Journal<'_> {
    let snapshot = model.snapshot();
    Journal::Ready {
        snapshot,
        log: snapshot.log(),
    }
}

/// One frame of `journal` at `now` in UTC, through the bounds check.
#[track_caller]
pub(crate) fn render_checked(
    journal: &Journal<'_>,
    screen: &Screen,
    now: Timestamp,
) -> (Buffer, Drawn) {
    draw_checked(screen, |screen, buf| {
        render(journal, screen, now, &TimeZone::UTC, buf)
    })
}

/// One frame of a journal that does not exist, through the bounds check.
#[track_caller]
pub(crate) fn render_missing(screen: &Screen) -> (Buffer, Drawn) {
    render_checked(&Journal::Missing, screen, FIXED_NOW)
}

/// A screen with the given view, size and profile, at the top of its view.
pub(crate) fn screen(view: View, width: u16, height: u16, colours: Colours, keys: bool) -> Screen {
    Screen {
        view,
        keys,
        scroll: 0,
        width,
        height,
        one_frame: false,
        colours,
        quit: Quit::Keys,
    }
}

/// Fails the test when `buf` breaks the frame rules of `screen`: see [`check_frame_bounds`].
#[track_caller]
pub(crate) fn assert_frame_bounds(buf: &Buffer, screen: &Screen) {
    if let Err(violation) = check_frame_bounds(buf, screen) {
        panic!("{violation}");
    }
}

/// Draws one frame into a fresh buffer of the frame area and asserts its bounds. Every frame test draws through
/// this, so bounds hold for every frame rendered.
#[track_caller]
pub(crate) fn draw_checked(
    screen: &Screen,
    draw: impl FnOnce(&Screen, &mut Buffer) -> Drawn,
) -> (Buffer, Drawn) {
    let mut buf = Buffer::empty(frame_area(screen));
    let drawn = draw(screen, &mut buf);
    assert_frame_bounds(&buf, screen);
    (buf, drawn)
}

/// The symbols of each row of `buf`, untrimmed.
pub(crate) fn rows_of(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

/// Fails the test when `buf` shows a glyph the console font cannot draw, or a `\u` escape of one.
#[track_caller]
pub(crate) fn assert_glyphs_drawable(buf: &Buffer, what: &str) {
    for (row, text) in rows_of(buf).iter().enumerate() {
        for c in text.chars() {
            assert!(
                is_console_glyph(c),
                "{what} row {row} draws {c:?} outside IBM437: {text:?}"
            );
        }
        assert!(
            !text.contains(r"\u"),
            "{what} row {row} escapes a glyph the console cannot draw: {text:?}"
        );
    }
}

/// The text of `buf`, one `│`-led row per line, trailing blanks trimmed.
pub(crate) fn text_snapshot(buf: &Buffer) -> String {
    let mut out = String::new();
    for row in rows_of(buf) {
        let _ = writeln!(out, "│{}", row.trim_end());
    }
    out
}

const LETTERS: [(Paint, char); 12] = [
    (Paint::Text, 't'),
    (Paint::Grey, 'g'),
    (Paint::White, 'w'),
    (Paint::Running, 'c'),
    (Paint::Hunt, 'a'),
    (Paint::Good, 'p'),
    (Paint::Bad, 'f'),
    (Paint::Combination, 'm'),
    (Paint::Track, 'k'),
    (Paint::Chip, 'h'),
    (Paint::Lamp, 'l'),
    (Paint::Current, 'u'),
];

fn paints(cell: &Cell, style: Style) -> bool {
    cell.fg == style.fg.unwrap_or(Color::Reset)
        && cell.bg == style.bg.unwrap_or(Color::Reset)
        && cell.modifier == style.add_modifier
}

fn letter(cell: &Cell, colours: Colours) -> char {
    if cell.fg == Color::Reset && cell.bg == Color::Reset && cell.modifier.is_empty() {
        return ' ';
    }
    LETTERS
        .iter()
        .find(|&&(tone, _)| paints(cell, tone.style(colours)))
        .map_or('?', |&(_, letter)| letter)
}

/// The text of `buf` with the tone of every styled cell under it: each row as `│text`, followed by a `~` row of
/// tone letters when any cell of the row is styled. A frame with no styling has no `~` rows.
pub(crate) fn styled_snapshot(buf: &Buffer, colours: Colours) -> String {
    let legend: Vec<String> = LETTERS
        .iter()
        .map(|&(tone, letter)| format!("{letter}={tone:?}"))
        .collect();
    let mut out = format!("legend: {}\n", legend.join(" "));
    for (y, row) in rows_of(buf).iter().enumerate() {
        let _ = writeln!(out, "│{}", row.trim_end());
        let Ok(y) = u16::try_from(y) else { break };
        let letters: String = (0..buf.area.width)
            .map(|x| letter(&buf[(x, y)], colours))
            .collect();
        if !letters.trim().is_empty() {
            let _ = writeln!(out, "~{}", letters.trim_end());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};

    use super::{
        assert_frame_bounds, assert_glyphs_drawable, draw_checked, screen, styled_snapshot,
        text_snapshot,
    };
    use crate::frames::colours::Paint;
    use crate::frames::draw::put;
    use crate::screen::{Colours, Drawn, Hold, View};
    use ratatui::text::Line;

    #[test]
    #[should_panic(expected = "the frame area")]
    fn a_buffer_the_size_of_the_screen_fails_the_bounds_check() {
        let sc = screen(View::Main, 10, 5, Colours::Full, false);
        assert_frame_bounds(&Buffer::empty(Rect::new(0, 0, 10, 5)), &sc);
    }

    #[test]
    #[should_panic(expected = "outside IBM437")]
    fn a_glyph_outside_the_console_font_fails_the_glyph_check() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        buf[(0, 0)].set_symbol("✓");
        assert_glyphs_drawable(&buf, "test");
    }

    #[test]
    fn the_snapshots_show_text_and_tones() {
        let sc = screen(View::Main, 9, 3, Colours::Full, false);
        let (buf, _) = draw_checked(&sc, |sc, buf| {
            put(
                buf,
                Rect::new(0, 0, 8, 2),
                0,
                0,
                &Line::from(vec![
                    Paint::Bad.span(sc.colours, "ab"),
                    "cd".into(),
                    Paint::Grey.span(sc.colours, "e"),
                ]),
            );
            Drawn {
                scroll: 0,
                hold: Hold::Never,
            }
        });
        assert_eq!(text_snapshot(&buf), "│abcde\n│\n");
        let styled = styled_snapshot(&buf, Colours::Full);
        assert!(styled.ends_with("│abcde\n~ff  g\n│\n"), "{styled}");
        let mut plain = Buffer::empty(Rect::new(0, 0, 3, 1));
        plain[(0, 0)].set_symbol("x");
        assert!(styled_snapshot(&plain, Colours::None).ends_with("│x\n"));
        plain[(1, 0)]
            .set_fg(Color::Red)
            .set_style(ratatui::style::Style::new().add_modifier(Modifier::BOLD));
        assert!(styled_snapshot(&plain, Colours::Full).contains("~ ?"));
    }
}
