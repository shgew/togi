//! Placing styled lines into a buffer, clipped to a rectangle.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use unicode_width::UnicodeWidthChar as _;

use super::colours::Paint;
use super::fit::{cut_styled, truncate_styled};
use crate::screen::Colours;

/// Writes `line` at `(x, y)` inside `rect`, cut to the cells that remain by whole words with a cut mark. Cells
/// outside `rect` or the buffer are never touched; what is under the line is replaced, styles included.
pub fn put(buf: &mut Buffer, rect: Rect, x: u16, y: u16, line: &Line<'_>) {
    place(buf, rect, x, y, line, true);
}

/// Like [`put`], but the line is clipped by cells, inside words too. For gauges and rule dashes.
pub fn put_cells(buf: &mut Buffer, rect: Rect, x: u16, y: u16, line: &Line<'_>) {
    place(buf, rect, x, y, line, false);
}

fn place(buf: &mut Buffer, rect: Rect, x: u16, y: u16, line: &Line<'_>, words: bool) {
    let visible = rect.intersection(buf.area);
    let (Some(at_x), Some(at_y)) = (rect.x.checked_add(x), rect.y.checked_add(y)) else {
        return;
    };
    if !visible.contains(Position::new(at_x, at_y)) {
        return;
    }
    let room = usize::from(visible.right() - at_x);
    let fitted = if words {
        cut_styled(line, room)
    } else {
        truncate_styled(line, room)
    };
    let mut at = at_x;
    for span in &fitted.spans {
        for c in span.content.chars() {
            if c.width().unwrap_or(0) == 0 || at >= visible.right() {
                continue;
            }
            let cell = &mut buf[(at, at_y)];
            cell.reset();
            cell.set_char(c);
            cell.set_style(Style::new().patch(span.style));
            at += 1;
        }
    }
}

/// `lines` for a panel of `height` rows. Lines that do not fit end in a grey `+N more` that counts the lines it
/// replaces.
#[must_use]
pub fn bounded_rows(
    mut lines: Vec<Line<'static>>,
    height: usize,
    colours: Colours,
) -> Vec<Line<'static>> {
    if height == 0 {
        return Vec::new();
    }
    if lines.len() > height {
        let hidden = lines.len() - height + 1;
        lines.truncate(height - 1);
        lines.push(Line::styled(
            format!("+{hidden} more"),
            Paint::Grey.style(colours),
        ));
    }
    lines
}

/// Writes `lines` down `rect`, one per row, with `+N more` for what does not fit.
pub fn rows(buf: &mut Buffer, rect: Rect, lines: Vec<Line<'static>>, colours: Colours) {
    for (y, line) in bounded_rows(lines, usize::from(rect.height), colours)
        .iter()
        .enumerate()
    {
        let Ok(y) = u16::try_from(y) else { break };
        put(buf, rect, 0, y, line);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    use super::{bounded_rows, put, put_cells, rows};
    use crate::frames::fit::plain;
    use crate::screen::Colours;

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    #[test]
    fn put_clips_to_the_rectangle_by_whole_words() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        put(
            &mut buf,
            Rect::new(2, 0, 11, 1),
            0,
            0,
            &Line::from("so a freeze needs a reset"),
        );
        assert_eq!(row(&buf, 0), format!("  so a...{}", " ".repeat(11)));
        assert_eq!(row(&buf, 1).trim(), "");
    }

    #[test]
    fn put_cells_clips_inside_words() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
        put_cells(&mut buf, Rect::new(0, 0, 5, 1), 1, 0, &Line::from("██████"));
        assert_eq!(row(&buf, 0), " ████   ");
    }

    #[test]
    fn nothing_is_written_outside_the_rectangle_or_the_buffer() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        put_cells(
            &mut buf,
            Rect::new(4, 1, 10, 5),
            0,
            0,
            &Line::from("abcdefgh"),
        );
        put(&mut buf, Rect::new(0, 0, 6, 3), 9, 0, &Line::from("zzz"));
        put(&mut buf, Rect::new(0, 0, 6, 3), 0, 7, &Line::from("zzz"));
        put(
            &mut buf,
            Rect::new(u16::MAX, 0, 6, 3),
            1,
            0,
            &Line::from("zzz"),
        );
        assert_eq!(row(&buf, 0).trim(), "");
        assert_eq!(row(&buf, 1), "    ab");
        assert_eq!(row(&buf, 2).trim(), "");
    }

    #[test]
    fn a_later_placement_replaces_the_cells_under_it_with_their_styles() {
        let red = Style::new().fg(Color::Red);
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        put(
            &mut buf,
            Rect::new(0, 0, 6, 1),
            0,
            0,
            &Line::from(Span::styled("abcdef", red)),
        );
        put(&mut buf, Rect::new(0, 0, 6, 1), 1, 0, &Line::from("XY"));
        assert_eq!(row(&buf, 0), "aXYdef");
        assert_eq!(buf[(0, 0)].fg, Color::Red);
        assert_eq!(buf[(1, 0)].fg, Color::Reset);
        assert_eq!(buf[(3, 0)].fg, Color::Red);
    }

    #[test]
    fn untrusted_text_is_written_escaped() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        put(
            &mut buf,
            Rect::new(0, 0, 20, 1),
            0,
            0,
            &Line::from("a\x1b[2Jb"),
        );
        assert_eq!(row(&buf, 0).trim_end(), r"a\x1b[2Jb");
    }

    #[test]
    fn rows_that_do_not_fit_end_in_a_count_of_what_they_leave_out() {
        let lines: Vec<Line<'static>> = (1..=6).map(|n| Line::from(format!("line {n}"))).collect();
        let bounded = bounded_rows(lines.clone(), 4, Colours::None);
        assert_eq!(
            bounded.iter().map(plain).collect::<Vec<_>>(),
            ["line 1", "line 2", "line 3", "+3 more"]
        );
        assert_eq!(bounded_rows(lines.clone(), 6, Colours::None).len(), 6);
        assert!(bounded_rows(lines.clone(), 0, Colours::None).is_empty());
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 4));
        rows(&mut buf, Rect::new(0, 0, 10, 2), lines, Colours::None);
        assert_eq!(row(&buf, 1).trim(), "+5 more");
        assert_eq!(row(&buf, 2).trim(), "");
    }
}
