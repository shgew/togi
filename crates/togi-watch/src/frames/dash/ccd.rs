//! One CCD's table: a heading with the scale, the CCD's committed D, S and U, how much of the cap its coverage
//! backs, and a row per core with the offset read back and a gauge.
//!
//! The rows show what the tuner's projection says; nothing here computes a cap, an edge or a distance.

use ratatui::buffer::Buffer;
use ratatui::text::{Line, Span};
use togi_journal::{Binding, Edge, Slope};
use togi_machine::TargetOffset;
use togi_tuner::policy::{CapProjection, UnitProjection};

use crate::frames::colours::Paint;
use crate::frames::draw::{put, put_cells};
use crate::frames::fit::{Clause, Form, cut_words, fit_clauses};
use crate::frames::measure::Ccd;
use crate::frames::scene::Scene;
use crate::model::CoreView;
use crate::numbers::saturating_cells;
use crate::screen::Colours;

/// The word a core's row carries for its place in the running trial.
pub(super) fn state_word(scene: &Scene<'_>, core: &CoreView) -> &'static str {
    if !scene.trial_runs() {
        ""
    } else if core.loaded {
        "LOADED"
    } else {
        ""
    }
}

/// The offset a row shows: the saved profile once a stopped session restored its offsets, else the readback,
/// never clamped.
fn shown_offset(scene: &Scene<'_>, core: &CoreView) -> Option<i64> {
    if scene.s.stopped.is_some_and(|stop| stop.saved) {
        Some(core.profile.get())
    } else {
        core.applied.map(|offset| offset.0)
    }
}

/// The glyphs a gauge draws applied depth and the edge with. The help legend and examples use the same set.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Marks {
    pub(crate) applied: char,
    pub(crate) edge: char,
}

/// The marks of a colour profile: the console's half-height bars, or the rich profile's three-quarter-height bar
/// for the applied depth and the edge (told apart by colour).
pub(crate) const fn marks(colours: Colours) -> Marks {
    match colours {
        Colours::Rich => Marks {
            applied: '▆',
            edge: '▆',
        },
        Colours::Full | Colours::Console | Colours::None => Marks {
            applied: '▄',
            edge: '▄',
        },
    }
}

/// The count a gauge marks as the CCD's edge, and whether it is an observed failure or a censored limit.
fn edge_mark(ccd: Option<&UnitProjection>) -> Option<(i64, Paint)> {
    match ccd?.edge.as_ref()? {
        Edge::Observed { offset, .. } => Some((offset.get(), Paint::Bad)),
        Edge::Censored { limit, .. } => Some((limit.get(), Paint::Warn)),
        Edge::Unknown {} => None,
    }
}

/// The gauge of a core in `cells` cells, 2 counts to a cell at 25 cells and 1 at 50: the applied depth, the
/// cap's reach as a dotted track, and the CCD's edge. `applied` is the offset it draws as applied.
pub(super) fn gauge(
    scene: &Scene<'_>,
    core: &CoreView,
    ccd: Option<&UnitProjection>,
    applied: Option<i64>,
    cells: usize,
) -> Line<'static> {
    if cells == 0 {
        return Line::default();
    }
    let cap = ccd
        .and_then(|ccd| ccd.cap.as_ref())
        .map_or(0, |cap| cap.offset.get());
    let edge = edge_mark(ccd);
    let marks = marks(scene.colours);
    let total = i64::from(saturating_cells(cells));
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_glyph = ' ';
    let mut run_paint = None;
    for i in 0..total {
        let (lo, hi) = (i * 50 / total + 1, (i + 1) * 50 / total);
        let hits = |point: i64| (lo..=hi).contains(&-point);
        let (glyph, paint) = if let Some((_, paint)) = edge.filter(|&(at, _)| hits(at)) {
            (marks.edge, Some(paint))
        } else if applied.is_some_and(|applied| -applied >= lo) {
            (
                marks.applied,
                Some(if core.loaded {
                    Paint::White
                } else {
                    Paint::Grey
                }),
            )
        } else if -cap >= lo {
            ('·', Some(Paint::Grey))
        } else {
            (' ', None)
        };
        if (glyph, paint) != (run_glyph, run_paint) && !run.is_empty() {
            spans.push(span(scene, run_paint, std::mem::take(&mut run)));
        }
        (run_glyph, run_paint) = (glyph, paint);
        run.push(glyph);
    }
    spans.push(span(scene, run_paint, run));
    Line::from(spans)
}

fn span(scene: &Scene<'_>, paint: Option<Paint>, text: String) -> Span<'static> {
    match paint {
        Some(paint) => scene.paint(paint, text),
        None => Span::raw(text),
    }
}

fn slope_word(slope: &Slope) -> &'static str {
    match slope {
        Slope::Fallback { .. } => "fallback",
        Slope::Measured { .. } => "measured",
    }
}

fn binding_words(binding: &Binding) -> String {
    match binding {
        Binding::Rate { class } => format!("rate {}", class.regime),
        Binding::EdgeMargin {} => "edge margin".to_owned(),
        Binding::Baseline {} => "baseline".to_owned(),
    }
}

fn offset_text(offset: TargetOffset) -> String {
    offset.get().to_string()
}

/// What U says: unknown, the offset that failed, or the limit no failure was seen down to.
pub(crate) fn edge_words(edge: Option<&Edge>) -> (String, String) {
    match edge {
        None | Some(Edge::Unknown {}) => ("U unknown".to_owned(), "U ?".to_owned()),
        Some(Edge::Observed { offset, .. }) => (
            format!("U fails at {}", offset.get()),
            format!("U {}", offset.get()),
        ),
        Some(Edge::Censored { limit, .. }) => (
            format!("U censored at {}", limit.get()),
            format!("U >{}", limit.get()),
        ),
    }
}

/// What S says, in full and short: its offset and slope label.
pub(crate) fn cap_words(cap: &CapProjection) -> (String, String) {
    let s = offset_text(cap.offset);
    (
        format!("S {s} {} slope", slope_word(&cap.slope)),
        format!("S {s}"),
    )
}

/// The distance row: committed D/S/binding, the separate current calculation, U and distance.
pub(super) fn facts_line(scene: &Scene<'_>, ccd: &UnitProjection, width: usize) -> Line<'static> {
    let sep = || scene.paint(Paint::Track, " · ");
    let text = |paint: Paint, value: String| Line::from(scene.paint(paint, value));
    let mut clauses = vec![Clause::whole(
        Line::default(),
        Line::from(vec![
            scene.paint(Paint::Grey, "D "),
            scene.paint(Paint::White, offset_text(ccd.day)),
        ]),
    )];
    if let Some(cap) = &ccd.cap {
        let (full, short) = cap_words(cap);
        clauses.push(Clause::new(
            sep(),
            vec![
                Form::new(text(Paint::Text, full), 3),
                Form::new(text(Paint::Text, short), 0),
            ],
        ));
        let bound = binding_words(&cap.binding);
        clauses.push(Clause::new(
            sep(),
            vec![
                Form::new(text(Paint::Grey, format!("bound by {bound}")), 4),
                Form::new(text(Paint::Grey, bound), 2),
                Form::empty(),
            ],
        ));
        if let Some(evidence) = &ccd.evidence {
            let offset = offset_text(evidence.offset);
            clauses.push(Clause::new(
                sep(),
                vec![
                    Form::new(
                        text(
                            Paint::Grey,
                            format!("calc {offset}: {}", evidence.reason.label()),
                        ),
                        0,
                    ),
                    Form::new(text(Paint::Grey, format!("calc {offset}")), 1),
                ],
            ));
        }
    } else {
        let reason = ccd
            .evidence
            .as_ref()
            .map_or("no cap yet", |evidence| evidence.reason.label());
        clauses.push(Clause::new(
            sep(),
            vec![
                Form::new(text(Paint::Grey, format!("S 0 · {reason}")), 2),
                Form::new(text(Paint::Grey, reason.to_owned()), 0),
            ],
        ));
    }
    let (edge_full, edge_short) = edge_words(ccd.edge.as_ref());
    let edge_paint = match ccd.edge {
        Some(Edge::Observed { .. }) => Paint::Bad,
        Some(Edge::Censored { .. }) => Paint::Warn,
        Some(Edge::Unknown {}) | None => Paint::Grey,
    };
    clauses.push(Clause::new(
        sep(),
        vec![
            Form::new(text(edge_paint, edge_full), 1),
            Form::new(text(edge_paint, edge_short), 0),
        ],
    ));
    if let Some(remaining) = ccd.remaining {
        let (full, short) = if remaining == 0 {
            ("at S".to_owned(), "at S".to_owned())
        } else {
            (
                format!("{remaining} counts to S"),
                format!("{remaining} to S"),
            )
        };
        clauses.push(Clause::new(
            sep(),
            vec![
                Form::new(text(Paint::Text, full), 2),
                Form::new(text(Paint::Text, short), 1),
                Form::empty(),
            ],
        ));
    }
    fit_clauses(width, &clauses, scene.colours)
}

/// The cap owner's eligible mapping witness hours and gaps; non-mapping exposure is not coverage.
pub(super) fn coverage_line(
    scene: &Scene<'_>,
    ccd: &UnitProjection,
    width: usize,
) -> Line<'static> {
    let text = |value: String| Line::from(scene.paint(Paint::Grey, value));
    let (full, short) = match &ccd.evidence {
        Some(evidence) => {
            let seconds = evidence
                .cells
                .iter()
                .fold(0_u64, |total, cell| total.saturating_add(cell.eligible_s));
            let non_mapping = evidence
                .uncovered
                .iter()
                .filter(|gap| gap.reason == togi_journal::UncoveredReason::NonMappingOnly)
                .count();
            let hours = seconds / 360;
            (
                format!(
                    "Ruleset 13 mapping: {}.{}h eligible · {} gaps ({} non-mapping)",
                    hours / 10,
                    hours % 10,
                    evidence.uncovered.len(),
                    non_mapping
                ),
                format!(
                    "Ruleset 13: {}.{}h · {} gaps",
                    hours / 10,
                    hours % 10,
                    evidence.uncovered.len()
                ),
            )
        }
        None => (
            "Ruleset 13 mapping: eligible hours/gaps unknown".to_owned(),
            "Ruleset 13: hours/gaps unknown".to_owned(),
        ),
    };
    fit_clauses(
        width,
        &[Clause::new(
            Line::default(),
            vec![Form::new(text(full), 1), Form::new(text(short), 0)],
        )],
        scene.colours,
    )
}

/// What this trial does with a CCD's cores, in a word.
pub(super) fn ccd_role(scene: &Scene<'_>, id: togi_machine::CcdId) -> String {
    let runs = scene.trial_runs();
    let (mut loaded, mut total) = (0, 0);
    for core in scene.s.cores.iter().filter(|core| core.ccd == id) {
        total += 1;
        loaded += usize::from(core.loaded && runs);
    }
    let role = match () {
        () if total == 0 => String::new(),
        () if loaded == total => "all loaded".into(),
        () if loaded == 0 => "idle".into(),
        () if loaded == 1 => "one loaded".into(),
        () => format!("{loaded} loaded"),
    };
    #[cfg(test)]
    crate::corpus::text("dash.role", "short", &role);
    role
}

/// Draws one CCD's table.
pub(crate) fn draw_ccd(buf: &mut Buffer, ccd: &Ccd, scene: &Scene<'_>) {
    let (r, col, id) = (ccd.rect, ccd.columns, ccd.id);
    let width = usize::from(r.width);
    let colours = scene.colours;
    let paint = |p: Paint, text: String| Line::from(scene.paint(p, text));
    let name = format!("CCD {}", id.get());
    let room = col.gauge.saturating_sub(name.len() + 3);
    let head = Line::from(vec![
        scene.paint(Paint::White, name),
        Span::raw(" "),
        scene.paint(Paint::Grey, cut_words(&ccd_role(scene, id), room, colours)),
    ]);
    put(buf, r, 0, 0, &head, colours);
    put(
        buf,
        r,
        saturating_cells(col.gauge.saturating_sub(1)),
        0,
        &paint(Paint::Grey, "0".into()),
        colours,
    );
    if col.cells == 50 {
        for n in (10..=50).step_by(10) {
            put(
                buf,
                r,
                saturating_cells((col.gauge + n).saturating_sub(3)),
                0,
                &paint(Paint::Grey, format!("-{n}")),
                colours,
            );
        }
    } else {
        put(
            buf,
            r,
            saturating_cells(col.gauge + 2),
            0,
            &paint(Paint::Track, "2 counts/cell".into()),
            colours,
        );
        put(
            buf,
            r,
            saturating_cells((col.gauge + col.cells).saturating_sub(3)),
            0,
            &paint(Paint::Grey, "-50".into()),
            colours,
        );
    }
    let projection = scene
        .s
        .policy
        .units
        .iter()
        .find(|p| p.unit == togi_tuner::unit::UnitId::Ccd(id));
    match projection {
        Some(projection) => {
            put(buf, r, 0, 1, &facts_line(scene, projection, width), colours);
            put(
                buf,
                r,
                0,
                2,
                &coverage_line(scene, projection, width),
                colours,
            );
        }
        None => put(
            buf,
            r,
            0,
            1,
            &paint(Paint::Grey, "no tuner state for this CCD yet".into()),
            colours,
        ),
    }
    let cores: Vec<&CoreView> = scene.s.cores.iter().filter(|core| core.ccd == id).collect();
    let rows = usize::from(r.height.saturating_sub(3));
    let mut shown = cores.len().min(rows);
    if cores.len() > shown && shown > 0 {
        shown -= 1;
        let more = paint(Paint::Grey, format!("+{} more", cores.len() - shown));
        put(buf, r, 0, r.height - 1, &more, colours);
    }
    for (i, core) in cores.iter().take(shown).enumerate() {
        let y = saturating_cells(i + 3);
        let applied = shown_offset(scene, core);
        let own = if core.loaded {
            Paint::White
        } else {
            Paint::Text
        };
        if core.loaded && scene.trial_runs() {
            put(buf, r, 0, y, &paint(Paint::Running, "►".into()), colours);
        }
        put(
            buf,
            r,
            saturating_cells(col.id),
            y,
            &paint(own, format!("{:02}", core.id.get())),
            colours,
        );
        put(
            buf,
            r,
            saturating_cells(col.offset),
            y,
            &paint(
                own,
                applied.map_or_else(|| "  ?".to_owned(), |offset| format!("{offset:3}")),
            ),
            colours,
        );
        put(
            buf,
            r,
            saturating_cells(col.state),
            y,
            &paint(Paint::Text, state_word(scene, core).into()),
            colours,
        );
        put_cells(
            buf,
            r,
            saturating_cells(col.gauge),
            y,
            &gauge(scene, core, projection, applied, col.cells),
            colours,
        );
    }
}
