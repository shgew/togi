//! Where things sit: the size class, the frame area and the rectangles every view shares.
//!
//! The frame area is `(width - 1) x (height - 1)`, so the last row and the last column of
//! the terminal are never written and the terminal never scrolls.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr as _;

use crate::screen::Screen;

/// How much the screen has room for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SizeClass {
    /// Narration is shortened and the lower panels give way to the core tables.
    Compact,
    /// Width 150 and height 40 or more: the two CCD tables sit side by side and the countdown is inline.
    Medium,
    /// Width 200 and height 60 or more: a three-row trial bar and large remaining-time digits.
    Wide,
}

impl SizeClass {
    /// The class of a terminal of `width` columns and `height` rows.
    #[must_use]
    pub const fn of(width: u16, height: u16) -> Self {
        if width >= 200 && height >= 60 {
            Self::Wide
        } else if width >= 150 && height >= 40 {
            Self::Medium
        } else {
            Self::Compact
        }
    }
}

/// The area a frame may write: the screen less its last row and last column.
#[must_use]
pub const fn frame_area(screen: &Screen) -> Rect {
    Rect::new(
        0,
        0,
        screen.width.saturating_sub(1),
        screen.height.saturating_sub(1),
    )
}

/// The rows of the dashboard that depend only on the size class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rows {
    /// The stage line.
    pub stage: u16,
    /// The narration.
    pub say: u16,
    /// The trial band.
    pub now: u16,
    /// The first CCD table.
    pub ccd: u16,
    /// The height of the trial band.
    pub now_height: u16,
    /// The outcome lines.
    pub outcomes: u16,
    /// The height of the outcome lines.
    pub outcomes_height: u16,
}

/// The rectangles every view shares, measured from the screen alone. Panels that depend on the model are
/// measured by the panels from these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The size class.
    pub class: SizeClass,
    /// The frame area: nothing is written outside it.
    pub area: Rect,
    /// Cells between the area's left edge and every panel.
    pub margin: u16,
    /// Cells between two panels side by side.
    pub gap: u16,
    /// The width inside the margins.
    pub full: u16,
    /// The width of a left or right half.
    pub left: u16,
    /// Where the right half starts.
    pub right_x: u16,
    /// The row classes place by size.
    pub rows: Rows,
    /// The footer row, `area.height - 1`.
    pub hint: u16,
    /// The header: clock, tuning time, session age and counts.
    pub header: Rect,
    /// The stage line.
    pub stage: Rect,
    /// The narration, beside the last outcome unless compact.
    pub say: Rect,
    /// The last outcome, right of the narration; empty width when compact.
    pub last: Rect,
    /// Between the narration and the footer: where help and the journal view draw.
    pub body: Rect,
    /// The key hints.
    pub footer: Rect,
}

impl Layout {
    /// The layout of `screen`.
    #[must_use]
    pub fn new(screen: &Screen) -> Self {
        let area = frame_area(screen);
        let class = SizeClass::of(screen.width, screen.height);
        let (margin, gap, rows) = match class {
            SizeClass::Wide => (
                2,
                5,
                Rows {
                    stage: 2,
                    say: 4,
                    now: 7,
                    ccd: 19,
                    now_height: 7,
                    outcomes: 15,
                    outcomes_height: 3,
                },
            ),
            SizeClass::Medium => (
                2,
                5,
                Rows {
                    stage: 2,
                    say: 4,
                    now: 7,
                    ccd: 15,
                    now_height: 3,
                    outcomes: 11,
                    outcomes_height: 3,
                },
            ),
            SizeClass::Compact => (
                1,
                3,
                Rows {
                    stage: 1,
                    say: 3,
                    now: 5,
                    ccd: 10,
                    now_height: 2,
                    outcomes: 8,
                    outcomes_height: 1,
                },
            ),
        };
        let mut full = area.width.saturating_sub(2 * margin);
        let mut left = full.saturating_sub(gap) / 2;
        let mut right_x = margin + left + gap;
        if class == SizeClass::Wide && full >= 235 {
            (left, right_x, full) = (115, 122, 235);
        }
        let hint = area.height.saturating_sub(1);
        let say_height = if class == SizeClass::Compact { 1 } else { 2 };
        let say_width = if class == SizeClass::Compact {
            full
        } else {
            left
        };
        Self {
            class,
            area,
            margin,
            gap,
            full,
            left,
            right_x,
            rows,
            hint,
            header: Rect::new(margin, 0, full, 1),
            stage: Rect::new(margin, rows.stage, full, 1),
            say: Rect::new(margin, rows.say, say_width, say_height),
            last: Rect::new(right_x, rows.say, left, 2),
            body: Rect::new(margin, rows.say, full, hint.saturating_sub(rows.say + 1)),
            footer: Rect::new(margin, hint, full, 1),
        }
    }
}

/// Checks that a frame buffer follows the frame rules for `screen`: its area is exactly [`frame_area`], so no
/// cell lies in the last row or column of the screen, and no wide glyph sticks out of the right edge. Buffer
/// cells cannot be set outside the buffer's area, so a frame that draws through the buffer cannot write outside
/// it.
///
/// # Errors
/// Names the first rule the buffer breaks.
pub fn check_frame_bounds(buf: &Buffer, screen: &Screen) -> Result<(), String> {
    let want = frame_area(screen);
    if buf.area != want {
        return Err(format!(
            "{}x{}: the frame area is {:?}, want {want:?}",
            screen.width, screen.height, buf.area
        ));
    }
    if u32::from(buf.area.right()) >= u32::from(screen.width).max(1)
        || u32::from(buf.area.bottom()) >= u32::from(screen.height).max(1)
    {
        return Err(format!(
            "{}x{}: the frame reaches the last row or column",
            screen.width, screen.height
        ));
    }
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let symbol = buf[(x, y)].symbol();
            if symbol.width() > 1 && x + 1 >= buf.area.width {
                return Err(format!(
                    "{}x{}: a wide glyph {symbol:?} at ({x}, {y}) sticks out of the frame",
                    screen.width, screen.height
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use test_case::test_case;

    use super::{Layout, SizeClass, check_frame_bounds, frame_area};
    use crate::frames::testing::screen;
    use crate::screen::{Colours, View};

    #[test_case(240, 67, SizeClass::Wide)]
    #[test_case(200, 60, SizeClass::Wide)]
    #[test_case(199, 60, SizeClass::Medium)]
    #[test_case(200, 59, SizeClass::Medium)]
    #[test_case(160, 45, SizeClass::Medium)]
    #[test_case(150, 40, SizeClass::Medium)]
    #[test_case(149, 40, SizeClass::Compact)]
    #[test_case(150, 39, SizeClass::Compact)]
    #[test_case(120, 33, SizeClass::Compact)]
    #[test_case(80, 24, SizeClass::Compact)]
    #[test_case(0, 0, SizeClass::Compact)]
    #[test_case(300, 20, SizeClass::Compact)]
    fn the_size_classes_break_at_their_widths_and_heights(
        width: u16,
        height: u16,
        want: SizeClass,
    ) {
        assert_eq!(SizeClass::of(width, height), want, "{width}x{height}");
    }

    #[test_case(240, 67, (239, 66); "240 by 67")]
    #[test_case(80, 24, (79, 23); "80 by 24")]
    #[test_case(1, 1, (0, 0); "1 by 1")]
    #[test_case(0, 0, (0, 0); "0 by 0")]
    #[test_case(1, 9, (0, 8); "1 by 9")]
    fn the_frame_area_is_the_screen_less_its_last_row_and_column(
        width: u16,
        height: u16,
        want: (u16, u16),
    ) {
        let area = frame_area(&screen(View::Main, width, height, Colours::Full, false));
        assert_eq!(
            (area.x, area.y, area.width, area.height),
            (0, 0, want.0, want.1),
            "{width}x{height}"
        );
    }

    #[test]
    fn the_standard_sizes_measure_as_the_spec_describes() {
        let wide = Layout::new(&screen(View::Main, 240, 67, Colours::Full, true));

        assert_eq!(
            (wide.margin, wide.gap, wide.full, wide.left, wide.right_x),
            (2, 5, 235, 115, 122)
        );
        assert_eq!(wide.body, Rect::new(2, 4, 235, 60));
        assert_eq!(wide.footer, Rect::new(2, 65, 235, 1));
        let medium = Layout::new(&screen(View::Main, 160, 45, Colours::Full, true));
        assert_eq!((medium.full, medium.left, medium.right_x), (155, 75, 82));
        assert_eq!(medium.body, Rect::new(2, 4, 155, 38));
        let compact = Layout::new(&screen(View::Main, 80, 24, Colours::Full, true));
        assert_eq!((compact.margin, compact.gap, compact.full), (1, 3, 77));
        assert_eq!(compact.body, Rect::new(1, 3, 77, 18));
        assert_eq!(compact.say, Rect::new(1, 3, 77, 1));
        assert_eq!(compact.footer.y, 22);
    }

    #[test]
    fn every_rectangle_lies_inside_the_frame_area_at_every_size() {
        for width in 0..=260 {
            for height in [0, 1, 2, 3, 5, 12, 24, 33, 45, 67] {
                let layout = Layout::new(&screen(View::Main, width, height, Colours::Full, true));
                for (name, rect) in [
                    ("header", layout.header),
                    ("footer", layout.footer),
                    ("body", layout.body),
                ] {
                    if rect.width == 0 || rect.height == 0 || layout.area.height == 0 {
                        continue;
                    }
                    assert!(
                        layout
                            .area
                            .contains(ratatui::layout::Position::new(rect.x, rect.y)),
                        "{width}x{height} {name} starts outside: {rect:?}"
                    );
                    assert!(
                        rect.right() <= layout.area.right().max(rect.x),
                        "{width}x{height} {name} ends outside: {rect:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_bounds_check_accepts_the_frame_area() {
        let sc = screen(View::Main, 80, 24, Colours::Full, false);
        assert_eq!(
            check_frame_bounds(&Buffer::empty(frame_area(&sc)), &sc),
            Ok(())
        );
    }

    #[test_case(Rect::new(0, 0, 80, 24); "the whole screen")]
    #[test_case(Rect::new(0, 0, 80, 23); "a row short")]
    #[test_case(Rect::new(0, 0, 79, 24); "a column short")]
    #[test_case(Rect::new(1, 0, 79, 23); "shifted right")]
    #[test_case(Rect::new(0, 0, 78, 23); "two columns short")]
    fn the_bounds_check_rejects_any_other_area(area: Rect) {
        let sc = screen(View::Main, 80, 24, Colours::Full, false);
        assert!(
            check_frame_bounds(&Buffer::empty(area), &sc).is_err(),
            "{area:?}"
        );
    }

    #[test]
    fn the_bounds_check_rejects_a_wide_glyph_on_the_last_cell() {
        let sc = screen(View::Main, 8, 4, Colours::Full, false);
        let mut buf = Buffer::empty(frame_area(&sc));
        buf[(6, 0)].set_symbol("界");
        assert!(check_frame_bounds(&buf, &sc).is_err());
        buf[(6, 0)].set_symbol(" ");
        buf[(5, 0)].set_symbol("界");
        assert_eq!(check_frame_bounds(&buf, &sc), Ok(()));
    }
}
