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

/// The narrowest CCD table, in cells, that fits a 50-cell gauge from cell 20.
const DENSE_WIDTH: usize = 71;

/// The rows a CCD table spends above its cores: the heading with the scale, the distance row and the coverage
/// row.
const CCD_HEAD_ROWS: usize = 3;

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
    /// The context panel: the night's task, progress and shares, or the day's hold, step and events.
    pub(crate) context: Rect,
    /// What happened; no rows when the CCD tables do not sit side by side.
    pub(crate) history: Rect,
}

fn rect(x: u16, y: u16, w: usize, h: usize) -> Rect {
    let w = crate::numbers::saturating_cells(w);
    let h = crate::numbers::saturating_cells(h);
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
    let ccd_y = if scene.s.resting() && !scene.s.between_trials() {
        now_height = resting_band(scene).lines.len();
        if class != SizeClass::Compact {
            now_height += 1;
        }
        outcomes_height = 0;
        usize::from(rows.now) + now_height + 1
    } else {
        usize::from(rows.ccd)
    };
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
        let core_phase = scene
            .s
            .policy
            .climb
            .units
            .iter()
            .any(|unit| matches!(unit.unit, togi_tuner::unit::UnitId::Core(_)));
        let core_rows = cores * if core_phase { 2 } else { 1 };
        let mut height = (core_rows + CCD_HEAD_ROWS).min(hint.saturating_sub(y + 1));
        if !side {
            let share = (hint.saturating_sub(ccd_y + 3) / ids.len().max(1)).max(CCD_HEAD_ROWS);
            height = height.min(share);
        }
        let mut columns = match class {
            SizeClass::Medium | SizeClass::Compact if w >= DENSE_WIDTH => CoreColumns {
                id: 2,
                offset: 6,
                state: 11,
                gauge: 20,
                cells: 50,
            },
            SizeClass::Wide => CoreColumns {
                id: 2,
                offset: 6,
                state: 11,
                gauge: 21,
                cells: 50,
            },
            SizeClass::Medium => CoreColumns {
                id: 2,
                offset: 6,
                state: 11,
                gauge: 21,
                cells: 25,
            },
            SizeClass::Compact if !side => CoreColumns {
                id: 2,
                offset: 6,
                state: 11,
                gauge: 21,
                cells: 25,
            },
            SizeClass::Compact => CoreColumns {
                id: 2,
                offset: 5,
                state: 9,
                gauge: 18,
                cells: 25,
            },
        };
        columns.cells = columns.cells.min(w.saturating_sub(columns.gauge + 1));
        let x = crate::numbers::saturating_cells(x);
        let top = crate::numbers::saturating_cells(y);
        ccds.push(Ccd {
            id,
            rect: rect(x, top, w, height),
            columns,
        });
        low_y = low_y.max(y + height + 1);
    }
    let context_height = hint.saturating_sub(low_y + 1);
    let low = crate::numbers::saturating_cells(low_y);
    let (context, history) = if side {
        (
            rect(margin, low, left, context_height),
            rect(layout.right_x, low, left, context_height),
        )
    } else {
        let below = crate::numbers::saturating_cells(low_y + context_height);
        (
            rect(margin, low, full, context_height),
            rect(margin, below, full, 0),
        )
    };
    Measure {
        now: rect(margin, rows.now, full, now_height),
        outcomes: rect(margin, rows.outcomes, full, outcomes_height),
        ccds,
        context,
        history,
        layout,
    }
}
