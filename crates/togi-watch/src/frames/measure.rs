//! Where the main view's model-dependent panels sit: the trial band, the outcome lines, a table per CCD, the
//! context panel and the history, measured from the screen's [`Layout`] and the snapshot.
//!
//! Every live trial state (running, preparing, between trials) gives each panel the same rectangle: between trials
//! the trial band's slot holds the resting text and the outcome lines keep the last trial's outcomes. A stop, a
//! dead end, a crash or a recovery takes the rows its band needs and the outcome lines give theirs up, so the
//! panels below move up rather than leaving blank rows.

use ratatui::layout::Rect;
use togi_machine::CcdId;

use super::layout::{Layout, SizeClass};
use super::scene::Scene;
use super::story::resting_band;
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

/// The narrowest CCD table, in cells, that fits a 50-cell gauge: the gauge from cell 20, a four-cell failure
/// point from cell 71, and no per-core note.
const DENSE_WIDTH: usize = 75;

/// The columns of a CCD's core table, as cells from the table's left edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CoreColumns {
    /// The core id.
    pub(crate) id: usize,
    /// The applied offset.
    pub(crate) offset: usize,
    /// The state word.
    pub(crate) state: usize,
    /// The gauge.
    pub(crate) gauge: usize,
    /// The gauge's cells.
    pub(crate) cells: usize,
    /// The failure point.
    pub(crate) failure: usize,
    /// The cells the failure point is right-aligned in.
    pub(crate) failure_width: usize,
    /// The note.
    pub(crate) note: usize,
    /// The role sits beside the CCD name and notes are bare numbers.
    pub(crate) brief: bool,
}

/// One CCD's core table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ccd {
    /// The CCD.
    pub(crate) id: CcdId,
    /// Where its table draws: a heading row, then a row per core.
    pub(crate) rect: Rect,
    /// Its columns.
    pub(crate) columns: CoreColumns,
}

/// The main view's rectangles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Measure {
    /// What depends on the screen alone.
    pub(crate) layout: Layout,
    /// The trial band, or the resting band: the same slot while a trial runs, is prepared or is awaited.
    pub(crate) now: Rect,
    /// The outcome lines; no rows while stopped, at a dead end, crashed or recovering.
    pub(crate) outcomes: Rect,
    /// A table per CCD, in CCD order.
    pub(crate) ccds: Vec<Ccd>,
    /// The context panel: BIOS, cycle, hunt, turns, round, combinations, R7.
    pub(crate) context: Rect,
    /// What happened; no rows when the CCD tables do not sit side by side.
    pub(crate) history: Rect,
    /// The context tables' columns.
    pub(crate) tables: Tables,
}

fn rect(x: u16, y: u16, w: usize, h: usize) -> Rect {
    let w = u16::try_from(w).unwrap_or(u16::MAX);
    let h = u16::try_from(h).unwrap_or(u16::MAX);
    Rect::new(x, y, w, h)
}

/// The main view of `scene` on `screen`.
pub(crate) fn measure(scene: &Scene<'_>, screen: &Screen) -> Measure {
    let layout = Layout::new(screen);
    let class = layout.class;
    let rows = layout.rows;
    let hint = usize::from(layout.hint);
    let margin = layout.margin;
    let (full, left, gap) = (
        usize::from(layout.full),
        usize::from(layout.left),
        usize::from(layout.gap),
    );
    let mut now_height = usize::from(rows.now_height);
    let mut outcomes_height = usize::from(rows.outcomes_height);
    let mut ccd_y = usize::from(rows.ccd);
    if scene.s.resting() && !scene.s.between_trials() {
        now_height = resting_band(scene).lines.len();
        if class != SizeClass::Compact {
            now_height += 1;
        }
        outcomes_height = 0;
        ccd_y = usize::from(rows.now) + now_height + 1;
    }
    let mut ids: Vec<CcdId> = scene.s.cores.iter().map(|core| core.ccd).collect();
    ids.sort_unstable();
    ids.dedup();
    let side = left >= 50;
    let mut low_y = ccd_y;
    let mut ccds = Vec::with_capacity(ids.len());
    for (i, &id) in ids.iter().enumerate() {
        let cores = scene.s.cores.iter().filter(|core| core.ccd == id).count();
        let (x, y, w) = if side {
            let x = usize::from(margin) + (i % 2) * (left + gap);
            (x, if i >= 2 { low_y } else { ccd_y }, left)
        } else {
            (usize::from(margin), low_y, full)
        };
        let mut height = (cores + 1).min(hint.saturating_sub(y + 1));
        if !side {
            let share = (hint.saturating_sub(ccd_y + 3) / ids.len().max(1)).max(1);
            height = height.min(share);
        }
        let mut columns = match class {
            SizeClass::Wide => CoreColumns {
                id: 2,
                offset: 6,
                state: 11,
                gauge: 21,
                cells: 50,
                failure: 73,
                failure_width: 5,
                note: 81,
                brief: false,
            },
            SizeClass::Medium => CoreColumns {
                id: 2,
                offset: 6,
                state: 11,
                gauge: 21,
                cells: 25,
                failure: 48,
                failure_width: 5,
                note: 56,
                brief: false,
            },
            SizeClass::Compact if !side => CoreColumns {
                id: 2,
                offset: 6,
                state: 11,
                gauge: 21,
                cells: 25,
                failure: 48,
                failure_width: 5,
                note: 56,
                brief: false,
            },
            SizeClass::Compact => CoreColumns {
                id: 2,
                offset: 5,
                state: 9,
                gauge: 18,
                cells: 25,
                failure: 44,
                failure_width: 5,
                note: 50,
                brief: true,
            },
        };
        if class != SizeClass::Wide && w >= DENSE_WIDTH {
            columns = CoreColumns {
                id: 2,
                offset: 6,
                state: 11,
                gauge: 20,
                cells: 50,
                failure: 71,
                failure_width: 4,
                note: w,
                brief: true,
            };
        }
        columns.cells = columns.cells.min(w.saturating_sub(columns.gauge + 1));
        let x = u16::try_from(x).unwrap_or(u16::MAX);
        let top = u16::try_from(y).unwrap_or(u16::MAX);
        ccds.push(Ccd {
            id,
            rect: rect(x, top, w, height),
            columns,
        });
        low_y = low_y.max(y + height + 1);
    }
    let context_height = hint.saturating_sub(low_y + 1);
    let low = u16::try_from(low_y).unwrap_or(u16::MAX);
    let (context, history) = if side {
        (
            rect(margin, low, left, context_height),
            rect(layout.right_x, low, left, context_height),
        )
    } else {
        let below = u16::try_from(low_y + context_height).unwrap_or(u16::MAX);
        (
            rect(margin, low, full, context_height),
            rect(margin, below, full, 0),
        )
    };
    Measure {
        now: rect(margin, rows.now, full, now_height),
        outcomes: rect(margin, rows.outcomes, full, outcomes_height),
        ccds,
        tables: Tables::new(class, usize::from(context.width)),
        context,
        history,
        layout,
    }
}
