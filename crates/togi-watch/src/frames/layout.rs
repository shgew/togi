//! Where things sit: the size class, the frame area and the rectangles every page shares.
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
    /// Under 160 columns or 45 rows: Main is the numeric table, not the gauges.
    Compact,
    /// 160 columns and 45 rows or more: Main draws the two 77-cell CCD gauges.
    Medium,
    /// 200 columns and 60 rows or more: the same gauges at the same scale; the extra room goes to What happened
    /// and the detail tables.
    Wide,
}

impl SizeClass {
    /// The class of a terminal of `width` columns and `height` rows.
    #[must_use]
    pub const fn of(width: u16, height: u16) -> Self {
        if width >= 200 && height >= 60 {
            Self::Wide
        } else if width >= 160 && height >= 45 {
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

/// The rectangles every page shares, measured from the screen alone. Main's own rows are in
/// [`Main`](super::measure::Main).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The size class.
    pub class: SizeClass,
    /// The frame area: nothing is written outside it.
    pub area: Rect,
    /// Cells between the area's left edge and every panel; the same on the right.
    pub margin: u16,
    /// The width inside the margins.
    pub full: u16,
    /// The footer row, `area.height - 1`.
    pub hint: u16,
    /// The header: clock, tuning time, session age and counts.
    pub header: Rect,
    /// The stage line.
    pub stage: Rect,
    /// The page title under the stage line: on every page but Main at the reference size, where the row is blank.
    pub title: Rect,
    /// Between the title and the footer: a page's scrolled body, the help, the journal and the narration of a
    /// journal without a session.
    pub body: Rect,
    /// The key hints.
    pub footer: Rect,
}

impl Layout {
    /// The layout of `screen`.
    #[must_use]
    pub fn new(screen: &Screen) -> Self {
        let area = frame_area(screen);
        let margin = 1;
        let full = area.width.saturating_sub(2 * margin);
        let hint = area.height.saturating_sub(1);
        Self {
            class: SizeClass::of(screen.width, screen.height),
            area,
            margin,
            full,
            hint,
            header: Rect::new(margin, 0, full, 1),
            stage: Rect::new(margin, 1, full, u16::from(hint > 1)),
            title: Rect::new(margin, 2, full, u16::from(hint > 2)),
            body: Rect::new(margin, 3, full, hint.saturating_sub(3)),
            footer: Rect::new(margin, hint, full, 1),
        }
    }
}

/// How many rows the scrolled body of `screen`'s page has: the rows under the title and above the footer. The key
/// handling pages by it; Main at the reference size does not scroll.
#[must_use]
pub fn body_rows(screen: &Screen) -> usize {
    usize::from(Layout::new(screen).body.height)
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

    use super::{Layout, SizeClass, body_rows, check_frame_bounds, frame_area};
    use crate::frames::testing::screen;
    use crate::screen::{Colours, View};

    #[test]
    fn the_size_classes_break_at_their_widths_and_heights() {
        for (width, height, want) in [
            (240, 67, SizeClass::Wide),
            (200, 60, SizeClass::Wide),
            (199, 60, SizeClass::Medium),
            (200, 59, SizeClass::Medium),
            (160, 45, SizeClass::Medium),
            (159, 45, SizeClass::Compact),
            (160, 44, SizeClass::Compact),
            (150, 40, SizeClass::Compact),
            (120, 33, SizeClass::Compact),
            (80, 24, SizeClass::Compact),
            (0, 0, SizeClass::Compact),
            (300, 20, SizeClass::Compact),
        ] {
            assert_eq!(SizeClass::of(width, height), want, "{width}x{height}");
        }
    }

    #[test]
    fn the_frame_area_is_the_screen_less_its_last_row_and_column() {
        for (width, height, want) in [
            (240, 67, (239, 66)),
            (80, 24, (79, 23)),
            (1, 1, (0, 0)),
            (0, 0, (0, 0)),
            (1, 9, (0, 8)),
        ] {
            let area = frame_area(&screen(View::Main, width, height, Colours::Full, false));
            assert_eq!(
                (area.x, area.y, area.width, area.height),
                (0, 0, want.0, want.1),
                "{width}x{height}"
            );
        }
    }

    #[test]
    fn the_reference_size_keeps_one_margin_column_each_side() {
        let reference = Layout::new(&screen(View::Main, 160, 45, Colours::Full, true));
        assert_eq!((reference.margin, reference.full), (1, 157));
        assert_eq!(reference.header, Rect::new(1, 0, 157, 1));
        assert_eq!(reference.stage, Rect::new(1, 1, 157, 1));
        assert_eq!(reference.title, Rect::new(1, 2, 157, 1));
        assert_eq!(reference.footer, Rect::new(1, 43, 157, 1));
        assert_eq!(reference.body, Rect::new(1, 3, 157, 40));
        let wide = Layout::new(&screen(View::Main, 240, 67, Colours::Full, true));
        assert_eq!(wide.footer, Rect::new(1, 65, 237, 1));
        let compact = Layout::new(&screen(View::Main, 80, 30, Colours::Full, true));
        assert_eq!(compact.footer, Rect::new(1, 28, 77, 1));
        assert_eq!(compact.body, Rect::new(1, 3, 77, 25));
    }

    #[test]
    fn the_body_rows_are_the_rows_under_the_title() {
        for (width, height, want) in [(160, 45, 40), (80, 30, 25), (79, 29, 24), (240, 67, 62)] {
            for view in View::ALL {
                let sc = screen(view, width, height, Colours::Full, true);
                assert_eq!(body_rows(&sc), want, "{view:?} {width}x{height}");
            }
        }
        assert_eq!(
            body_rows(&screen(View::Cores, 10, 2, Colours::Full, true)),
            0
        );
    }

    #[test]
    fn every_rectangle_lies_inside_the_frame_area_at_every_size() {
        for width in 0..=260 {
            for height in [0, 1, 2, 3, 5, 12, 24, 33, 45, 67] {
                let layout = Layout::new(&screen(View::Main, width, height, Colours::Full, true));
                for (name, rect) in [
                    ("header", layout.header),
                    ("stage", layout.stage),
                    ("title", layout.title),
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
    fn the_bounds_check_accepts_the_frame_area_and_nothing_else() {
        let sc = screen(View::Main, 80, 24, Colours::Full, false);
        assert_eq!(
            check_frame_bounds(&Buffer::empty(frame_area(&sc)), &sc),
            Ok(())
        );
        for area in [
            Rect::new(0, 0, 80, 24),
            Rect::new(0, 0, 80, 23),
            Rect::new(0, 0, 79, 24),
            Rect::new(1, 0, 79, 23),
            Rect::new(0, 0, 78, 23),
        ] {
            assert!(
                check_frame_bounds(&Buffer::empty(area), &sc).is_err(),
                "{area:?}"
            );
        }
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
