//! Where Main's rows sit, and the columns of the detail tables.
//!
//! Main's rectangles depend on the screen alone: a live trial state (running, preparing, between trials, stopped,
//! crashed, recovering) changes what a row says, never where it is.
//!
//! - Reference (160 columns by 45 rows or more): the exact row budget of the design, the two 77-cell CCD gauges
//!   side by side. Taller or wider screens only give What happened more rows and the explanation more room.
//! - Compact, at least 80 by 30: the same top rows, then the numeric core table, then What happened.
//! - Smaller: one scrolled body under the shared header, stage line and title.

use ratatui::layout::Rect;

use super::layout::{Layout, SizeClass};
use crate::screen::Screen;

/// A table column inside its panel: the cell it starts at and how many cells it may use. A column of width 0
/// is not shown at this size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Column {
    /// The first cell.
    pub(crate) at: usize,
    /// The cells it may use.
    pub(crate) w: usize,
}

const fn col(at: usize, w: usize) -> Column {
    Column { at, w }
}

const NONE: Column = col(0, 0);

/// Every context table's columns for the screen size, so a header and its rows always share them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tables {
    /// Where the value of a label/value row starts.
    pub(crate) field: usize,
    /// Marker, number, kind, where, workload, schedule, result.
    pub(crate) cycle: [Column; 7],
    /// Hunt parts: marker, part, at failing offsets, parked, schedule, state.
    pub(crate) parts: [Column; 6],
    /// Hunt groups: which, what, outcome.
    pub(crate) groups: [Column; 3],
    /// Member, now, failed at, passed at, state.
    pub(crate) probes: [Column; 5],
    /// The numbered steps after a hunt: number, text.
    pub(crate) steps: [Column; 2],
    /// Marker, core, this turn, at, workload, so far.
    pub(crate) turns: [Column; 6],
    /// Name, member offsets, found by, against the profile now.
    pub(crate) combos: [Column; 4],
    /// Cells per member offset in the combinations table.
    pub(crate) member: usize,
}

impl Tables {
    /// The columns of `class` in a context panel `width` cells wide.
    pub(crate) fn new(class: SizeClass, width: usize) -> Self {
        let rest = |at: usize| col(at, width.saturating_sub(at));
        match class {
            SizeClass::Wide => Self {
                field: 12,
                cycle: [
                    col(0, 1),
                    col(2, 2),
                    col(6, 21),
                    col(28, 23),
                    col(52, 28),
                    col(82, 13),
                    rest(97),
                ],
                parts: [
                    col(10, 1),
                    col(12, 10),
                    col(24, 19),
                    col(45, 12),
                    col(59, 9),
                    rest(70),
                ],
                groups: [col(12, 12), col(25, 44), rest(70)],
                probes: [col(12, 7), col(20, 5), col(26, 19), col(46, 22), rest(69)],
                steps: [col(12, 2), rest(15)],
                turns: [
                    col(0, 1),
                    col(2, 6),
                    col(9, 30),
                    col(40, 6),
                    col(47, 25),
                    rest(73),
                ],
                combos: [col(0, 5), col(6, 40), col(48, 21), rest(70)],
                member: 5,
            },
            SizeClass::Medium => Self {
                field: 12,
                cycle: [
                    col(0, 1),
                    col(2, 2),
                    col(5, 17),
                    col(23, 19),
                    NONE,
                    col(43, 13),
                    rest(57),
                ],
                parts: [
                    col(10, 1),
                    col(12, 7),
                    col(20, 12),
                    col(33, 12),
                    col(46, 8),
                    rest(55),
                ],
                groups: [col(12, 12), col(25, 29), rest(55)],
                probes: [col(12, 5), col(18, 5), col(24, 12), col(37, 17), rest(55)],
                steps: [col(12, 2), rest(15)],
                turns: [col(0, 1), col(2, 3), col(6, 27), col(34, 4), NONE, rest(39)],
                combos: [col(0, 3), col(4, 32), col(37, 7), rest(45)],
                member: 4,
            },
            SizeClass::Compact => Self {
                field: 11,
                cycle: [
                    col(0, 1),
                    col(2, 2),
                    col(5, 17),
                    col(23, 13),
                    NONE,
                    NONE,
                    rest(37),
                ],
                parts: [col(0, 1), col(2, 8), col(11, 25), NONE, NONE, rest(37)],
                groups: [col(0, 12), col(13, 23), rest(37)],
                probes: [col(2, 5), col(8, 4), col(13, 11), col(25, 11), rest(37)],
                steps: [col(11, 2), rest(14)],
                turns: [col(0, 1), col(2, 3), col(6, 26), col(33, 4), NONE, rest(38)],
                combos: [col(0, 3), NONE, col(4, 7), rest(12)],
                member: 0,
            },
        }
    }
}

/// The columns one CCD's gauge takes: three for the core id and its badges, one blank, 50 for the lane, one
/// blank, 22 for the values.
pub(crate) const GAUGE_WIDTH: u16 = 77;
/// The rows one CCD's gauge takes: a heading, the column labels and eight three-row core records.
pub(crate) const GAUGE_HEIGHT: u16 = 26;
/// The cells between the two gauges.
const GAUGE_GAP: u16 = 3;
/// The row the gauges start at.
const GAUGES_AT: u16 = 9;
/// The numeric table's rows: a heading and sixteen cores.
const TABLE_HEIGHT: u16 = 17;
/// The rows of What happened a scrolled Main shows under its table.
pub(crate) const SCROLLED_HISTORY_ROWS: usize = 4;

/// How Main is laid out at a screen size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MainForm {
    /// 160 by 45 or more: the two CCD gauges.
    Gauges,
    /// 80 by 30 or more, under the gauges' size: the numeric table in its own rows.
    Table,
    /// Under 80 by 30: one scrolled body.
    Scrolled,
}

/// Main's rectangles on a screen. A rectangle the form does not use has no cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Main {
    /// The form.
    pub(crate) form: MainForm,
    /// The phase headline and the requirement under it.
    pub(crate) progress: Rect,
    /// The NOW sentence and the trial row under it.
    pub(crate) now: Rect,
    /// The left and the right CCD's gauge.
    pub(crate) gauges: [Rect; 2],
    /// What the gauges' marks mean.
    pub(crate) explain: Rect,
    /// The numeric core table.
    pub(crate) table: Rect,
    /// The What happened heading; none when What happened has a single row.
    pub(crate) history_head: Rect,
    /// What happened's rows.
    pub(crate) history: Rect,
    /// The unread-journal warning.
    pub(crate) warning: Rect,
}

impl Main {
    /// Main on `screen`.
    pub(crate) fn new(screen: &Screen) -> Self {
        let layout = Layout::new(screen);
        let (margin, full, hint) = (layout.margin, layout.full, layout.hint);
        let none = Rect::default();
        let top = Self {
            form: MainForm::Scrolled,
            progress: Rect::new(margin, 3, full, 2),
            now: Rect::new(margin, 6, full, 2),
            gauges: [none; 2],
            explain: none,
            table: none,
            history_head: none,
            history: none,
            warning: none,
        };
        match layout.class {
            SizeClass::Medium | SizeClass::Wide => {
                let block = 2 * GAUGE_WIDTH + GAUGE_GAP;
                let left = margin + full.saturating_sub(block) / 2;
                let history_at = GAUGES_AT + GAUGE_HEIGHT + 3;
                Self {
                    form: MainForm::Gauges,
                    gauges: [
                        Rect::new(left, GAUGES_AT, GAUGE_WIDTH, GAUGE_HEIGHT),
                        Rect::new(
                            left + GAUGE_WIDTH + GAUGE_GAP,
                            GAUGES_AT,
                            GAUGE_WIDTH,
                            GAUGE_HEIGHT,
                        ),
                    ],
                    explain: Rect::new(left, GAUGES_AT + GAUGE_HEIGHT, block, 1),
                    history_head: Rect::new(margin, history_at - 1, full, 1),
                    history: Rect::new(
                        margin,
                        history_at,
                        full,
                        hint.saturating_sub(history_at + 1),
                    ),
                    warning: Rect::new(margin, hint.saturating_sub(1), full, 1),
                    ..top
                }
            }
            SizeClass::Compact if screen.width >= 80 && screen.height >= 30 => {
                let history_at = GAUGES_AT + TABLE_HEIGHT;
                let rows = hint.saturating_sub(history_at + 1);
                let headed = rows > 1;
                Self {
                    form: MainForm::Table,
                    table: Rect::new(margin, GAUGES_AT, full, TABLE_HEIGHT),
                    history_head: Rect::new(margin, history_at, full, u16::from(headed)),
                    history: Rect::new(
                        margin,
                        history_at + u16::from(headed),
                        full,
                        rows - u16::from(headed),
                    ),
                    warning: Rect::new(margin, hint - 1, full, 1),
                    ..top
                }
            }
            SizeClass::Compact => Self {
                progress: none,
                now: none,
                ..top
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;
    use test_case::test_case;

    use super::{Main, MainForm};
    use crate::frames::layout::Layout;
    use crate::frames::testing::screen;
    use crate::screen::{Colours, View};

    fn main_at(width: u16, height: u16) -> Main {
        Main::new(&screen(View::Main, width, height, Colours::Full, true))
    }

    fn inside(rect: Rect, outer: Rect) -> bool {
        rect.is_empty()
            || outer.contains(rect.as_position())
                && rect.right() <= outer.right()
                && rect.bottom() <= outer.bottom()
    }

    #[test]
    fn the_reference_size_has_the_designs_exact_rows_and_columns() {
        let m = main_at(160, 45);
        assert_eq!(m.form, MainForm::Gauges);
        assert_eq!(m.progress, Rect::new(1, 3, 157, 2));
        assert_eq!(m.now, Rect::new(1, 6, 157, 2));
        assert_eq!(
            m.gauges,
            [Rect::new(1, 9, 77, 26), Rect::new(81, 9, 77, 26)]
        );
        assert_eq!(m.explain, Rect::new(1, 35, 157, 1));
        assert_eq!(m.history_head, Rect::new(1, 37, 157, 1));
        assert_eq!(m.history, Rect::new(1, 38, 157, 4));
        assert_eq!(m.warning, Rect::new(1, 42, 157, 1));
        assert_eq!(
            Layout::new(&screen(View::Main, 160, 45, Colours::Full, true))
                .footer
                .y,
            43
        );
        assert!(m.table.is_empty());
    }

    #[test_case(160, 45; "160 by 45")]
    #[test_case(170, 50; "170 by 50")]
    #[test_case(200, 60; "200 by 60")]
    #[test_case(240, 67; "240 by 67")]
    #[test_case(300, 90; "300 by 90")]
    fn every_reference_rectangle_lies_inside_the_frame_area(width: u16, height: u16) {
        let sc = screen(View::Main, width, height, Colours::Full, true);
        let area = Layout::new(&sc).area;
        let m = Main::new(&sc);
        for rect in [
            m.progress,
            m.now,
            m.gauges[0],
            m.gauges[1],
            m.explain,
            m.history_head,
            m.history,
            m.warning,
        ] {
            assert!(
                inside(rect, area),
                "{width}x{height}: {rect:?} outside {area:?}"
            );
        }
    }

    #[test_case(200, 60; "200 by 60")]
    #[test_case(240, 67; "240 by 67")]
    fn the_gauges_keep_their_geometry_and_the_extra_room_goes_to_what_happened(
        width: u16,
        height: u16,
    ) {
        let reference = main_at(160, 45);
        let m = main_at(width, height);
        assert_eq!(m.form, MainForm::Gauges, "{width}x{height}");
        for gauge in m.gauges {
            assert_eq!((gauge.width, gauge.height), (77, 26));
        }
        assert_eq!(m.gauges[1].x - m.gauges[0].x, 80);
        assert_eq!(m.gauges[0].y, 9);
        assert_eq!(
            (
                m.progress.y,
                m.now.y,
                m.explain.y,
                m.history_head.y,
                m.history.y
            ),
            (3, 6, 35, 37, 38)
        );
        let layout = Layout::new(&screen(View::Main, width, height, Colours::Full, true));
        assert_eq!(m.history.bottom() + 1, layout.footer.y);
        assert!(m.history.height > reference.history.height);
    }

    #[test]
    fn extra_room_centres_the_gauges() {
        let centred = main_at(240, 67);
        assert_eq!(centred.gauges[0].x, 1 + (237 - 157) / 2);
    }

    #[test]
    fn a_screen_a_cell_under_the_reference_is_compact() {
        assert_eq!(main_at(159, 45).form, MainForm::Table);
        assert_eq!(main_at(160, 44).form, MainForm::Table);
        assert_eq!(main_at(160, 45).form, MainForm::Gauges);
        assert_eq!(main_at(80, 30).form, MainForm::Table);
        assert_eq!(main_at(79, 30).form, MainForm::Scrolled);
        assert_eq!(main_at(80, 29).form, MainForm::Scrolled);
        assert_eq!(main_at(79, 29).form, MainForm::Scrolled);
    }

    #[test]
    fn compact_main_at_80_by_30_has_the_designs_rows() {
        let m = main_at(80, 30);
        assert_eq!(m.progress, Rect::new(1, 3, 77, 2));
        assert_eq!(m.now, Rect::new(1, 6, 77, 2));
        assert_eq!(m.table, Rect::new(1, 9, 77, 17));
        assert!(m.history_head.is_empty());
        assert_eq!(m.history, Rect::new(1, 26, 77, 1));
        assert_eq!(m.warning, Rect::new(1, 27, 77, 1));
        assert_eq!(
            Layout::new(&screen(View::Main, 80, 30, Colours::Full, true))
                .footer
                .y,
            28
        );
    }

    #[test]
    fn more_compact_height_adds_what_happened_rows_under_a_heading() {
        let m = main_at(100, 40);
        assert_eq!(m.table, Rect::new(1, 9, 97, 17));
        assert_eq!(m.history_head, Rect::new(1, 26, 97, 1));
        assert_eq!(m.history, Rect::new(1, 27, 97, 10));
        assert_eq!(m.warning.y, 37);
    }

    #[test]
    fn below_80_by_30_main_is_one_scrolled_body() {
        let m = main_at(79, 29);
        assert!(m.table.is_empty() && m.history.is_empty() && m.progress.is_empty());
        assert!(m.gauges.iter().all(|rect| rect.is_empty()));
    }
}
