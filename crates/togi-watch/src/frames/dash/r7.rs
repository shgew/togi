use ratatui::text::{Line, Span};
use togi_machine::{CoreId, Regime, WorkloadId, workloads};

use crate::frames::colours::Paint;
use crate::frames::fit::{Clause, Form, cut_words, display_width, fit_clauses, line_width, tint};
use crate::frames::layout::SizeClass;
use crate::frames::measure::Measure;
use crate::frames::scene::Scene;
use crate::words::{core_ids, workload_display};

const BY_OFFSET: &str = " by offset";
const NOTE: &str = "observed, not a guarantee";

fn ids(cores: &[CoreId]) -> String {
    if cores.is_empty() {
        String::new()
    } else {
        core_ids(cores.iter().copied())
    }
}

/// A workload's top requesters fitted to `width`. Offset proxies keep their label when either list is cut, and
/// the measured list is cut first, so it never takes the offset label.
pub(super) fn top_cell(measured: &[CoreId], by_offset: &[CoreId], width: usize) -> String {
    let (m, o) = (ids(measured), ids(by_offset));
    if by_offset.is_empty() {
        return cut_words(if m.is_empty() { "-" } else { &m }, width);
    }
    if !measured.is_empty() {
        let both = format!("{m}; {o}{BY_OFFSET}");
        if display_width(&both) <= width {
            return both;
        }
        let room = width.saturating_sub(2 + display_width(&format!("{o}{BY_OFFSET}")));
        let kept = cut_words(&m, room);
        if !kept.is_empty() {
            return format!("{kept}; {o}{BY_OFFSET}");
        }
    }
    let kept = cut_words(&o, width.saturating_sub(BY_OFFSET.len()));
    if !kept.is_empty() {
        return format!("{kept}{BY_OFFSET}");
    }
    if width >= BY_OFFSET.len() - 1 {
        return "by offset".to_owned();
    }
    String::new()
}

fn dashes(scene: &Scene<'_>, n: usize) -> Span<'static> {
    scene.paint(Paint::Track, "─".repeat(n.max(1)))
}

/// Heads the R7 lines with their column names; a wide panel says the evidence is no guarantee.
pub(super) fn r7_rule(scene: &Scene<'_>, width: usize, name: usize, top: usize) -> Line<'static> {
    let grey = |text: &str| scene.paint(Paint::Grey, text.to_owned());
    let top_label = if top >= 20 { "top requesters" } else { "top" };
    let mut line = Line::from(vec![
        grey("R7"),
        Span::raw(" "),
        dashes(scene, name.saturating_sub(3)),
        Span::raw(" "),
        grey(top_label),
        Span::raw(" "),
        dashes(scene, top.saturating_sub(top_label.len() + 1)),
        Span::raw(" "),
        grey("self-sufficient"),
    ]);
    let spare = width.saturating_sub(line_width(&line) + 1);
    if spare >= NOTE.len() + 3 {
        line.spans.push(Span::raw(" "));
        line.spans.push(dashes(scene, spare - NOTE.len() - 1));
        line.spans.push(Span::raw(" "));
        line.spans.push(grey(NOTE));
        return line;
    }
    if spare > 0 {
        line.spans.push(Span::raw(" "));
        line.spans.push(dashes(scene, spare));
        return line;
    }
    if line_width(&line) <= width {
        return line;
    }
    let mut fitted = fit_clauses(
        width,
        &[
            Clause::whole("", grey("R7")),
            Clause::new(
                " ",
                vec![
                    Form::new(grey(top_label), 2),
                    Form::new(grey("top"), 1),
                    Form::empty(),
                ],
            ),
            Clause::new(
                " ",
                vec![Form::new(grey("self-sufficient"), 1), Form::empty()],
            ),
        ],
    );
    let spare = width.saturating_sub(line_width(&fitted) + 1);
    if spare > 0 {
        fitted.spans.push(Span::raw(" "));
        fitted.spans.push(dashes(scene, spare));
    }
    fitted
}

fn ascii_cell(text: &str, width: usize) -> String {
    let mut cell = cut_words(text, width);
    let pad = width.saturating_sub(display_width(&cell));
    cell.extend(std::iter::repeat_n(' ', pad));
    cell
}

pub(super) fn r7_rows(scene: &Scene<'_>, class: SizeClass, width: usize) -> Vec<Line<'static>> {
    if scene.s.cycle.is_none() || scene.s.r7.is_empty() {
        return Vec::new();
    }
    let (mut name, mut top) = (17, 17);
    match class {
        SizeClass::Wide if width >= 100 => (name, top) = (30, 24),
        SizeClass::Medium if width >= 70 => (name, top) = (24, 20),
        _ => {}
    }
    if width < name + top + 1 {
        name = name.min(width.saturating_sub(1 + BY_OFFSET.len() - 1));
        top = width - name - usize::from(name > 0).min(width - name);
    }
    let current = scene.s.r7_workload();
    let mut out = vec![r7_rule(scene, width, name, top)];
    for index in 0..workloads(Regime::R7).len() {
        let Some(id) = WorkloadId::new(Regime::R7, index) else {
            continue;
        };
        let (mut measured, mut by_offset, mut own) = (Vec::new(), Vec::new(), Vec::new());
        let mut total = 0;
        for status in scene.s.r7.iter().filter(|status| status.workload == id) {
            total += 1;
            if status.top_requester && status.offset_proxy {
                by_offset.push(status.core);
            } else if status.top_requester {
                measured.push(status.core);
            }
            if status.self_sufficient {
                own.push(status.core);
            }
        }
        if total == 0 {
            continue;
        }
        let mut label = workload_display(id).to_owned();
        if display_width(&label) > name.saturating_sub(1) {
            label = label
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
        }
        let mut sufficient = if own.len() == total {
            "all".to_owned()
        } else if own.is_empty() {
            "none yet".to_owned()
        } else {
            ids(&own)
        };
        if display_width(&sufficient) > width.saturating_sub(name + top + 2) {
            sufficient = format!("{} of {total} cores", own.len());
        }
        let paint = if current == Some(id) {
            Paint::Text
        } else {
            Paint::Grey
        };
        let line = fit_clauses(
            width,
            &[
                Clause::whole("", ascii_cell(&label, name)),
                Clause::whole(" ", ascii_cell(&top_cell(&measured, &by_offset, top), top)),
                Clause::new(" ", vec![Form::new(sufficient, 1), Form::empty()]),
            ],
        );
        out.push(tint(line, scene.style(paint)));
    }
    out
}

/// Every R7 workload's current top requesters and self-sufficient cores, under their own rule, for the context
/// panel; empty before checking has a cycle.
pub(crate) fn r7_lines(scene: &Scene<'_>, measure: &Measure) -> Vec<Line<'static>> {
    r7_rows(
        scene,
        measure.layout.class,
        usize::from(measure.context.width),
    )
}
