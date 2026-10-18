//! One frame of a recording, as text.

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use togi_journal::Event;
use togi_watch::frames::{frame_area, render};
use togi_watch::model::Model;
use togi_watch::screen::{Colours, Journal, Quit, Screen, View};

use crate::Error;
use crate::playback::offset;

/// What to draw.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub view: View,
    pub width: u16,
    pub height: u16,
    /// The rendering profile; coloured profiles keep ANSI styling in the text.
    pub colours: Colours,
    /// How long after the last event of the prefix the frame is drawn.
    pub after: SignedDuration,
}

/// The dashboard as of the first `shown` events (at least one) of `events`, drawn `frame.after` after the last of
/// them in `zone`, one line a row and a newline after each.
pub fn draw(
    events: &[Event],
    shown: usize,
    frame: &Frame,
    zone: &TimeZone,
) -> Result<String, Error> {
    let events = events.split_at(shown.max(1).min(events.len())).0;
    let mut model = Model::new();
    model.fold_batch(events).map_err(|error| Error::Fold {
        seq: events[error.index].seq.get(),
        source: error.source,
    })?;
    let snapshot = model.snapshot();
    let screen = Screen {
        view: frame.view,
        keys: false,
        scroll: if frame.view == View::Log {
            usize::MAX
        } else {
            0
        },
        width: frame.width,
        height: frame.height,
        one_frame: true,
        colours: frame.colours,
        quit: Quit::Keys,
    };
    let last = events
        .last()
        .map_or(jiff::Timestamp::UNIX_EPOCH, |last| last.time);
    let mut buffer = Buffer::empty(frame_area(&screen));
    render(
        &Journal::Ready {
            snapshot,
            log: snapshot.log(),
        },
        &screen,
        offset(last, frame.after),
        zone,
        &mut buffer,
    );
    Ok(text(&buffer, frame.colours != Colours::None))
}

pub(crate) fn text(buffer: &Buffer, colour: bool) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let row: Vec<&Cell> = (area.left()..area.right())
            .map(|x| &buffer[(x, y)])
            .collect();
        let end = row
            .iter()
            .rposition(|cell| !blank(cell, colour))
            .map_or(0, |last| last + 1);
        let mut style = Style::default();
        let mut skip = 0;
        for cell in &row[..end] {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            skip = ratatui::text::Span::raw(cell.symbol())
                .width()
                .saturating_sub(1);
            if colour {
                style.change(cell, &mut out);
            }
            out.push_str(cell.symbol());
        }
        if colour {
            style.change(&Cell::EMPTY, &mut out);
        }
        out.push('\n');
    }
    out
}

fn blank(cell: &Cell, colour: bool) -> bool {
    cell.symbol().trim().is_empty()
        && (!colour || (cell.bg == Color::Reset && cell.modifier.is_empty()))
}

/// The attributes the terminal currently has set while a row is written.
#[derive(Default)]
struct Style {
    fg: Color,
    bg: Color,
    modifier: Modifier,
}

impl Style {
    /// Writes the escape sequence that turns the current attributes into `cell`'s, if they differ.
    fn change(&mut self, cell: &Cell, out: &mut String) {
        if (self.fg, self.bg, self.modifier) == (cell.fg, cell.bg, cell.modifier) {
            return;
        }
        out.push_str("\x1b[0");
        for (flag, code) in [
            (Modifier::BOLD, "1"),
            (Modifier::DIM, "2"),
            (Modifier::ITALIC, "3"),
            (Modifier::UNDERLINED, "4"),
            (Modifier::REVERSED, "7"),
            (Modifier::CROSSED_OUT, "9"),
        ] {
            if cell.modifier.contains(flag) {
                out.push(';');
                out.push_str(code);
            }
        }
        colour(out, cell.fg, 30);
        colour(out, cell.bg, 40);
        out.push('m');
        *self = Self {
            fg: cell.fg,
            bg: cell.bg,
            modifier: cell.modifier,
        };
    }
}

/// Appends the code of `color` as a foreground (`base` 30) or a background (`base` 40).
fn colour(out: &mut String, color: Color, base: u8) {
    use std::fmt::Write as _;
    let indexed = match color {
        Color::Reset => return,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Indexed(index) => {
            let _ = write!(out, ";{};5;{index}", base + 8);
            return;
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{};2;{r};{g};{b}", base + 8);
            return;
        }
    };
    let code = if indexed < 8 {
        base + indexed
    } else {
        base + 60 + indexed - 8
    };
    let _ = write!(out, ";{code}");
}
