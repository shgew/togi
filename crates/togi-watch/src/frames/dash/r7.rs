use ratatui::text::{Line, Span};
use togi_machine::{CoreSet, Regime, WorkloadId, workloads};
use togi_tuner::R7CoreStatus;

use crate::frames::colours::Paint;
use crate::frames::fit::{
    Clause, Form, cut_words, display_width, fit_clauses, line_width, rule, tint, wrap_styled,
};
use crate::frames::layout::SizeClass;
use crate::frames::scene::Scene;
use crate::words::{core_ids, workload_display};

const BY_OFFSET: &str = " by offset";
const NOTE: &str = "observed, not a guarantee";

/// The R7 evidence of some cores: their top requesters by measurement and by offset, the self-sufficient ones and
/// how many there are.
struct Evidence {
    measured: CoreSet,
    by_offset: CoreSet,
    own: CoreSet,
    total: usize,
}

fn evidence<'a>(statuses: impl Iterator<Item = &'a R7CoreStatus>) -> Evidence {
    let mut evidence = Evidence {
        measured: CoreSet::default(),
        by_offset: CoreSet::default(),
        own: CoreSet::default(),
        total: 0,
    };
    for status in statuses {
        evidence.total += 1;
        if status.top_requester && status.offset_proxy {
            evidence.by_offset.insert(status.core);
        } else if status.top_requester {
            evidence.measured.insert(status.core);
        }
        if status.self_sufficient {
            evidence.own.insert(status.core);
        }
    }
    evidence
}

fn ids(cores: CoreSet) -> String {
    if cores.is_empty() {
        String::new()
    } else {
        core_ids(cores)
    }
}

/// A workload's top requesters fitted to `width`. Offset proxies keep their label when either list is cut, and
/// the measured list is cut first, so it never takes the offset label.
pub(super) fn top_cell(measured: CoreSet, by_offset: CoreSet, width: usize) -> String {
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
        let Evidence {
            measured,
            by_offset,
            own,
            total,
        } = evidence(scene.s.r7.iter().filter(|status| status.workload == id));
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
            ids(own)
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
                Clause::whole(" ", ascii_cell(&top_cell(measured, by_offset, top), top)),
                Clause::new(" ", vec![Form::new(sufficient, 1), Form::empty()]),
            ],
        );
        out.push(tint(line, scene.style(paint)));
    }
    #[cfg(test)]
    crate::corpus::lines("dash.r7", &format!("{class:?}"), &out);
    out
}

const EMPTY: &str =
    "No R7 evidence yet: request order and self-sufficiency appear once checking has a cycle.";
const RAIL: &str = "All loaded cores share one voltage rail, set by the loaded core with the highest voltage request. R7 attribution keeps a separate top-request group on each loaded CCD: the cores within 1 mV of that CCD's highest request, tied. An unattributed failure is charged to the affected CCD's top group first, and that group need not contain the core that sets the rail.";
const PROVENANCE: &str = "Request order is read from the telemetry recorded for each CCD's full part. `per offset order` marks a CCD where no measurement covers a loaded core, so the order is taken from the offsets instead.";
const SELF_SUFFICIENT: &str = "Self-sufficient means the core has been observed passing as a top requester at its current offset or deeper. That is evidence so far, not a guarantee against future failures.";
const CONDITIONAL: &str = "While this R7 trial runs, any outcome is conditional: its end may add request or clock telemetry that changes the top requesters, the backoff targets and the partials derived from them.";

/// The R7 evidence page in `width` cells: what the shared rail and the request order mean, then for every R7
/// workload and CCD its top requesters, where that order comes from and which cores are self-sufficient. Before
/// checking has a cycle it says so in one sentence.
pub(crate) fn r7_lines(scene: &Scene<'_>, width: usize, class: SizeClass) -> Vec<Line<'static>> {
    let text = scene.style(Paint::Text);
    let grey = scene.style(Paint::Grey);
    let track = scene.style(Paint::Track);
    if scene.s.cycle.is_none() || scene.s.r7.is_empty() {
        return wrap_styled(EMPTY, width, text);
    }
    let mut out = vec![
        rule(
            width,
            &Line::from(scene.paint(Paint::Grey, "R7 EVIDENCE")),
            &Line::from(scene.paint(Paint::Grey, NOTE)),
            track,
        ),
        Line::default(),
    ];
    for note in [RAIL, PROVENANCE, SELF_SUFFICIENT] {
        out.extend(wrap_styled(note, width, grey));
    }
    let r7_runs = scene
        .s
        .trial
        .is_some_and(|trial| trial.regime == Regime::R7)
        && scene.trial_runs();
    if r7_runs {
        out.push(Line::default());
        out.extend(wrap_styled(CONDITIONAL, width, grey));
    }
    out.push(Line::default());
    out.extend(r7_rows(scene, class, width));
    let current = scene.s.r7_workload();
    for index in 0..workloads(Regime::R7).len() {
        let Some(id) = WorkloadId::new(Regime::R7, index) else {
            continue;
        };
        let rows: Vec<_> = scene
            .s
            .r7
            .iter()
            .filter(|status| status.workload == id)
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push(Line::default());
        let name = match (current == Some(id), r7_runs) {
            (true, true) => format!("{} · loading now", workload_display(id)),
            (true, false) => format!("{} · current step", workload_display(id)),
            (false, _) => workload_display(id).to_owned(),
        };
        out.push(Line::from(scene.paint(Paint::Text, name)));
        let mut ccds: Vec<_> = rows.iter().map(|status| status.ccd).collect();
        ccds.dedup();
        for ccd in ccds {
            let Evidence {
                measured,
                by_offset,
                own,
                total,
            } = evidence(rows.iter().copied().filter(|status| status.ccd == ccd));
            let tied = if measured.len() > 1 { " · tied" } else { "" };
            let top = if !by_offset.is_empty() {
                format!(
                    "top requesters {} · per offset order",
                    ids(measured.iter().chain(by_offset).collect())
                )
            } else if measured.is_empty() {
                "no top requester recorded yet".to_owned()
            } else {
                format!("top requesters {}{tied} · per request order", ids(measured))
            };
            let sufficient = if own.len() == total {
                format!("self-sufficient: all {total} cores")
            } else if own.is_empty() {
                "self-sufficient: none yet".to_owned()
            } else {
                format!(
                    "self-sufficient: {} ({} of {} cores)",
                    ids(own),
                    own.len(),
                    total
                )
            };
            let head = format!("CCD {}  ", ccd.0);
            let indent = " ".repeat(head.len());
            let room = width.saturating_sub(head.len()).max(1);
            let mut body = wrap_styled(&top, room, text);
            body.extend(wrap_styled(&sufficient, room, grey));
            for (row, line) in body.into_iter().enumerate() {
                let lead = if row == 0 {
                    head.clone()
                } else {
                    indent.clone()
                };
                let mut spans = vec![scene.paint(Paint::Grey, lead)];
                spans.extend(line.spans);
                out.push(Line::from(spans));
            }
        }
    }
    #[cfg(test)]
    crate::corpus::lines("page.r7", &format!("{class:?}"), &out);
    out
}

#[cfg(test)]
mod tests;
