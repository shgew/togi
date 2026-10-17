//! Main: the phase and how far through it, what runs now, every core, and a short What happened.
//!
//! Every part comes from its own seam: [`phase_progress`], [`now_lines`], [`draw_core_overview`] or
//! [`core_table`], and [`main_history`]. This module only places them in the rectangles of [`Main`].

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use togi_machine::CcdId;

use super::colours::Paint;
use super::dash::{core_table, draw_core_overview, now_lines, phase_progress};
use super::draw::{put, rows};
use super::fit::fit_clauses;
use super::footer::skipped_clause;
use super::layout::Layout;
use super::measure::{Main, MainForm, SCROLLED_HISTORY_ROWS};
use super::panels::main_history;
use super::scene::Scene;
use super::scroll::scroll_body_with_bar;
use crate::screen::{Colours, Screen};

/// What the history heading says.
const HEADING: &str = "WHAT HAPPENED";

/// What a scrolled Main says before its table, so a reader knows the rows run past the screen.
const SCROLL_NOTE: &str = "compact · scroll for all cores";

/// Draws Main of a journal with a session into `buf` and returns how far it scrolls: only the form under 80 by 30
/// scrolls.
pub(super) fn draw(
    scene: &Scene<'_>,
    screen: &Screen,
    layout: &Layout,
    main: &Main,
    buf: &mut Buffer,
) -> usize {
    let colours = screen.colours;
    let width = usize::from(layout.full);
    match main.form {
        MainForm::Gauges => {
            top(scene, colours, main, buf);
            gauges(scene, colours, layout, main, buf);
            history(scene, colours, main, buf);
            warning(scene, colours, main, buf);
            0
        }
        MainForm::Table => {
            top(scene, colours, main, buf);
            rows(buf, main.table, core_table(scene, width), colours);
            history(scene, colours, main, buf);
            warning(scene, colours, main, buf);
            0
        }
        MainForm::Scrolled => {
            let inner = width.saturating_sub(2);
            let mut lines: Vec<Line<'static>> = phase_progress(scene, inner).into();
            lines.extend(now_lines(scene, inner));
            lines.push(Line::from(Paint::Grey.span(colours, SCROLL_NOTE)));
            lines.extend(core_table(scene, inner));
            lines.push(heading(colours));
            lines.extend(main_history(scene, inner, SCROLLED_HISTORY_ROWS));
            let (body, extent) = scroll_body_with_bar(
                &lines,
                width,
                usize::from(layout.body.height),
                screen.scroll,
                colours,
            );
            rows(buf, layout.body, body, colours);
            extent
        }
    }
}

/// Whether Main puts the skipped-journal warning on its own row, so the footer need not.
pub(super) fn has_warning_row(main: &Main) -> bool {
    !main.warning.is_empty()
}

fn top(scene: &Scene<'_>, colours: Colours, main: &Main, buf: &mut Buffer) {
    rows(
        buf,
        main.progress,
        phase_progress(scene, usize::from(main.progress.width)).into(),
        colours,
    );
    rows(
        buf,
        main.now,
        now_lines(scene, usize::from(main.now.width)).into(),
        colours,
    );
}

/// The cores one CCD's gauge draws.
const GAUGE_CORES: usize = 8;

/// The two CCDs when the gauges can draw all of their cores: exactly two CCDs of at most eight cores each.
fn gauge_pair(scene: &Scene<'_>) -> Option<[CcdId; 2]> {
    let mut ids: Vec<CcdId> = scene.s.cores.iter().map(|core| core.ccd).collect();
    ids.sort_unstable();
    ids.dedup();
    let [left, right] = ids[..] else {
        return None;
    };
    let fits =
        |id: CcdId| scene.s.cores.iter().filter(|core| core.ccd == id).count() <= GAUGE_CORES;
    (fits(left) && fits(right)).then_some([left, right])
}

/// The two gauges and what their marks mean. Any other topology has no gauge pair to place, so its cores show in
/// the numeric table over the gauges' rows.
fn gauges(scene: &Scene<'_>, colours: Colours, layout: &Layout, main: &Main, buf: &mut Buffer) {
    if let Some([left, right]) = gauge_pair(scene) {
        draw_core_overview(buf, main.gauges[0], scene, left);
        draw_core_overview(buf, main.gauges[1], scene, right);
        put(buf, main.explain, 0, 0, &explanation(colours));
    } else {
        let rect = Rect::new(
            layout.margin,
            main.gauges[0].y,
            layout.full,
            main.gauges[0].height + 1,
        );
        rows(
            buf,
            rect,
            core_table(scene, usize::from(rect.width)),
            colours,
        );
    }
}

/// What the gauge's marks mean. The `s=solo g=gap f=fail` key is in the gauges' own column labels.
fn explanation(colours: Colours) -> Line<'static> {
    let grey = |text: &'static str| Paint::Grey.span(colours, text);
    Line::from(vec![
        Paint::White.span(colours, "white"),
        grey("=tune · "),
        Paint::Bad.span(colours, "red point"),
        grey("=recorded failure · gap=counts shallower than solo"),
    ])
}

fn heading(colours: Colours) -> Line<'static> {
    Line::from(Paint::Text.span(colours, HEADING))
}

/// What happened: under its heading, or, when it has one row, after its name on that row.
fn history(scene: &Scene<'_>, colours: Colours, main: &Main, buf: &mut Buffer) {
    let width = usize::from(main.history.width);
    if main.history_head.is_empty() {
        let label = format!("{HEADING} · ");
        let mut line = main_history(scene, width.saturating_sub(label.chars().count()), 1)
            .into_iter()
            .next()
            .unwrap_or_default();
        line.spans.insert(0, Paint::Text.span(colours, label));
        put(buf, main.history, 0, 0, &line);
        return;
    }
    put(buf, main.history_head, 0, 0, &heading(colours));
    rows(
        buf,
        main.history,
        main_history(scene, width, usize::from(main.history.height)),
        colours,
    );
}

fn warning(scene: &Scene<'_>, colours: Colours, main: &Main, buf: &mut Buffer) {
    if let Some(skipped) = scene.s.skipped {
        let width = usize::from(main.warning.width);
        put(
            buf,
            main.warning,
            0,
            0,
            &fit_clauses(width, &[skipped_clause(skipped, colours)]),
        );
    }
}
