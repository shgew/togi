//! Placing styled lines into a buffer, clipped to a rectangle.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use unicode_width::{UnicodeWidthChar as _, UnicodeWidthStr as _};

use super::colours::Paint;
use super::fit::{cut_styled, truncate_styled};
use crate::screen::Colours;

/// Writes `line` at `(x, y)` inside `rect`, cut to the cells that remain by whole words with a cut mark. Cells
/// outside `rect` or the buffer are never touched; what is under the line is replaced, styles included.
pub fn put(buf: &mut Buffer, rect: Rect, x: u16, y: u16, line: &Line<'_>, colours: Colours) {
    place(buf, rect, x, y, line, true, colours);
}

/// Like [`put`], but the line is clipped by cells, inside words too. For gauges and rule dashes.
pub fn put_cells(buf: &mut Buffer, rect: Rect, x: u16, y: u16, line: &Line<'_>, colours: Colours) {
    place(buf, rect, x, y, line, false, colours);
}

/// Places the cells of `line` by display width: a double-width character takes two buffer cells, the second
/// left empty, and one that would not fit whole is never drawn. A zero-width character joins the cell before it.
fn place(
    buf: &mut Buffer,
    rect: Rect,
    x: u16,
    y: u16,
    line: &Line<'_>,
    words: bool,
    colours: Colours,
) {
    let visible = rect.intersection(buf.area);
    let (Some(at_x), Some(at_y)) = (rect.x.checked_add(x), rect.y.checked_add(y)) else {
        return;
    };
    if !visible.contains(Position::new(at_x, at_y)) {
        return;
    }
    let room = usize::from(visible.right() - at_x);
    let fitted = if words {
        cut_styled(line, room, colours)
    } else {
        truncate_styled(line, room, colours)
    };
    if at_x > visible.x && buf[(at_x - 1, at_y)].symbol().width() > 1 {
        buf[(at_x - 1, at_y)].reset();
    }
    let mut at = at_x;
    let mut last: Option<u16> = None;
    for span in &fitted.spans {
        let style = Style::new().patch(span.style);
        for c in span.content.chars() {
            let width = c.width().unwrap_or(0);
            if width == 0 {
                if let Some(prev) = last {
                    let cell = &mut buf[(prev, at_y)];
                    let joined = format!("{}{c}", cell.symbol());
                    cell.set_symbol(&joined);
                }
                continue;
            }
            let Ok(width) = u16::try_from(width) else {
                continue;
            };
            if at.saturating_add(width) > visible.right() {
                continue;
            }
            let cell = &mut buf[(at, at_y)];
            cell.reset();
            cell.set_char(c);
            cell.set_style(style);
            for covered in at + 1..at + width {
                let cell = &mut buf[(covered, at_y)];
                cell.reset();
                cell.set_style(style);
            }
            last = Some(at);
            at += width;
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
        put(buf, rect, 0, y, line, colours);
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
            Colours::Console,
        );
        assert_eq!(row(&buf, 0), format!("  so a...{}", " ".repeat(11)));
        assert_eq!(row(&buf, 1).trim(), "");
    }

    #[test]
    fn rich_put_cuts_with_a_one_cell_marker() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        put(
            &mut buf,
            Rect::new(2, 0, 11, 1),
            0,
            0,
            &Line::from("so a freeze needs a reset"),
            Colours::Rich,
        );
        assert_eq!(row(&buf, 0), format!("  so a…{}", " ".repeat(13)));
    }

    #[test]
    fn put_cells_clips_inside_words() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
        put_cells(
            &mut buf,
            Rect::new(0, 0, 5, 1),
            1,
            0,
            &Line::from("██████"),
            Colours::Console,
        );
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
            Colours::Console,
        );
        put(
            &mut buf,
            Rect::new(0, 0, 6, 3),
            9,
            0,
            &Line::from("zzz"),
            Colours::Console,
        );
        put(
            &mut buf,
            Rect::new(0, 0, 6, 3),
            0,
            7,
            &Line::from("zzz"),
            Colours::Console,
        );
        put(
            &mut buf,
            Rect::new(u16::MAX, 0, 6, 3),
            1,
            0,
            &Line::from("zzz"),
            Colours::Console,
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
            Colours::Console,
        );
        put(
            &mut buf,
            Rect::new(0, 0, 6, 1),
            1,
            0,
            &Line::from("XY"),
            Colours::Console,
        );
        assert_eq!(row(&buf, 0), "aXYdef");
        assert_eq!(buf[(0, 0)].fg, Color::Red);
        assert_eq!(buf[(1, 0)].fg, Color::Reset);
        assert_eq!(buf[(3, 0)].fg, Color::Red);
    }

    #[test]
    fn untrusted_text_is_written_escaped() {
        for colours in [Colours::Console, Colours::Rich] {
            let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
            put(
                &mut buf,
                Rect::new(0, 0, 20, 1),
                0,
                0,
                &Line::from("a\x1b[2Jb"),
                colours,
            );
            assert_eq!(row(&buf, 0).trim_end(), r"a\x1b[2Jb", "{colours:?}");
        }
    }

    #[test]
    fn the_console_escapes_what_rich_draws() {
        let mut console = Buffer::empty(Rect::new(0, 0, 8, 1));
        put_cells(
            &mut console,
            Rect::new(0, 0, 8, 1),
            0,
            0,
            &Line::from("日×"),
            Colours::Console,
        );
        assert_eq!(row(&console, 0), r"\u65e5x ");
        let mut rich = Buffer::empty(Rect::new(0, 0, 8, 1));
        put_cells(
            &mut rich,
            Rect::new(0, 0, 8, 1),
            0,
            0,
            &Line::from("日×"),
            Colours::Rich,
        );
        assert_eq!(rich[(0, 0)].symbol(), "日");
        assert_eq!(rich[(2, 0)].symbol(), "×");
    }

    #[test]
    fn a_double_width_character_takes_two_cells_and_is_never_split() {
        let red = Style::new().fg(Color::Red);
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 1));
        put_cells(
            &mut buf,
            Rect::new(0, 0, 5, 1),
            0,
            0,
            &Line::from(Span::styled("日本語", red)),
            Colours::Rich,
        );
        assert_eq!(buf[(0, 0)].symbol(), "日");
        assert_eq!(buf[(1, 0)].symbol(), " ");
        assert_eq!(
            buf[(1, 0)].fg,
            Color::Red,
            "the covered cell keeps the style"
        );
        assert_eq!(buf[(2, 0)].symbol(), "本");
        assert_eq!(
            (buf[(4, 0)].symbol(), buf[(4, 0)].fg),
            (" ", Color::Reset),
            "half of 語 would pass the rectangle, so none of it is drawn"
        );
        let mut narrow = Buffer::empty(Rect::new(0, 0, 4, 1));
        put_cells(
            &mut narrow,
            Rect::new(0, 0, 4, 1),
            3,
            0,
            &Line::from("日"),
            Colours::Rich,
        );
        assert_eq!(row(&narrow, 0).trim(), "");
    }

    #[test]
    fn overwriting_half_a_double_width_character_clears_the_other_half() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        let rect = Rect::new(0, 0, 4, 1);
        put_cells(&mut buf, rect, 0, 0, &Line::from("日"), Colours::Rich);
        put_cells(&mut buf, rect, 1, 0, &Line::from("x"), Colours::Rich);
        assert_eq!(row(&buf, 0), " x  ");
        assert_eq!(buf[(0, 0)].symbol(), " ", "the orphaned half is gone");
    }

    #[test]
    fn a_combining_mark_joins_the_cell_before_it() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        put_cells(
            &mut buf,
            Rect::new(0, 0, 4, 1),
            0,
            0,
            &Line::from("e\u{301}x"),
            Colours::Rich,
        );
        assert_eq!(buf[(0, 0)].symbol(), "e\u{301}");
        assert_eq!(buf[(1, 0)].symbol(), "x");
    }

    #[test]
    fn rows_that_do_not_fit_end_in_a_count_of_what_they_leave_out() {
        let lines: Vec<Line<'static>> = (1..=6).map(|n| Line::from(format!("line {n}"))).collect();
        let bounded = bounded_rows(lines.clone(), 4, Colours::None);
        assert_eq!(
            bounded
                .iter()
                .map(|line| plain(line, Colours::None))
                .collect::<Vec<_>>(),
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
