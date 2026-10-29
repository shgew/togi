//! Physical CCD topology with unit-generic D, hard ceiling C and tagged U.
//! The policy supplies every permission fact; diagnostic S never draws the gauge.

use ratatui::buffer::Buffer;
use ratatui::text::{Line, Span};
use togi_journal::Edge;
use togi_tuner::policy::{ClimbQualificationState, ClimbUnitProjection, ClimbUnitState};
use togi_tuner::unit::UnitId;

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
fn edge_mark(unit: Option<&ClimbUnitProjection>) -> Option<(i64, Paint)> {
    match &unit?.edge {
        Edge::Observed { offset, .. } => Some((offset.get(), Paint::Bad)),
        Edge::Censored { limit, .. } => Some((limit.get(), Paint::Warn)),
        Edge::Unknown {} => None,
    }
}

/// Applied/read-back depth, the hard ceiling's dotted track and tagged edge.
pub(super) fn gauge(
    scene: &Scene<'_>,
    core: &CoreView,
    ccd: Option<&ClimbUnitProjection>,
    applied: Option<i64>,
    cells: usize,
) -> Line<'static> {
    if cells == 0 {
        return Line::default();
    }
    let cap = ccd
        .and_then(|unit| unit.ceiling)
        .map_or(0, |ceiling| ceiling.get());
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

/// What U says: unknown, the offset that failed, or the limit no failure was seen down to.
pub(crate) fn edge_words(edge: Option<&Edge>) -> (String, String) {
    match edge {
        None | Some(Edge::Unknown {}) => ("U unknown".to_owned(), "U ?".to_owned()),
        Some(Edge::Observed { offset, .. }) => (
            format!("U observed at {}", offset.get()),
            format!("U observed {}", offset.get()),
        ),
        Some(Edge::Censored { limit, .. }) => (
            format!("U censored at {}", limit.get()),
            format!("U censored {}", limit.get()),
        ),
    }
}

/// Unit facts fit whole values; there is deliberately no remaining-to-S display.
pub(super) fn facts_line(
    scene: &Scene<'_>,
    unit: &ClimbUnitProjection,
    width: usize,
) -> Line<'static> {
    let text = |value: String| Line::from(scene.paint(Paint::Text, value));
    let (edge_full, edge_short) = edge_words(Some(&unit.edge));
    let ceiling = unit
        .ceiling
        .map_or_else(|| "C unknown".to_owned(), |c| format!("C {}", c.get()));
    let mut clauses = vec![
        Clause::whole("", text(format!("D {}", unit.applied.get()))),
        Clause::whole(" · ", text(ceiling)),
        Clause::new(
            " · ",
            vec![
                Form::new(text(edge_full), 1),
                Form::new(text(edge_short), 0),
            ],
        ),
    ];
    if let Some(remaining) = unit.remaining {
        clauses.push(Clause::new(
            " · ",
            vec![
                Form::new(text(format!("{remaining} counts to C")), 2),
                Form::new(text(format!("{remaining} to C")), 1),
                Form::empty(),
            ],
        ));
    }
    if let Some(diagnostic) = &unit.diagnostic_s {
        clauses.push(Clause::new(
            " · ",
            vec![
                Form::new(
                    text(format!("S {} diagnostic only", diagnostic.cap.offset.get())),
                    3,
                ),
                Form::empty(),
            ],
        ));
    }
    fit_clauses(width, &clauses, scene.colours)
}

/// Hold, exact-vector qualification and finish/held state, never statistical coverage as permission.
pub(super) fn coverage_line(
    scene: &Scene<'_>,
    unit: &ClimbUnitProjection,
    width: usize,
) -> Line<'static> {
    let text = |value: String| Line::from(scene.paint(Paint::Text, value));
    let qualification = unit.next.as_ref().map_or_else(
        || "no candidate".to_owned(),
        |next| {
            let state = match next.qualification.state {
                ClimbQualificationState::Missing => "qualification missing",
                ClimbQualificationState::InProgress => "qualifying",
                ClimbQualificationState::Complete { .. } => "exact qualified",
                ClimbQualificationState::Failed { .. } => "qualification blocked",
            };
            format!(
                "{state} · {} outstanding",
                next.qualification.outstanding.len()
            )
        },
    );
    let state = match &unit.state {
        ClimbUnitState::Climbing => "climbing".to_owned(),
        ClimbUnitState::Held { reason } => format!(
            "held: {}",
            reason
                .iter()
                .map(super::super::story::blocker_words)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ClimbUnitState::Finished {
            reason, last_clean, ..
        } => format!(
            "finished: {} · last clean {}",
            super::super::story::finish_words(reason),
            last_clean.get()
        ),
    };
    fit_clauses(
        width,
        &[
            Clause::whole("", text(state)),
            Clause::new(
                " · ",
                vec![
                    Form::new(
                        text(format!(
                            "hold {} / {} h",
                            unit.hold.credited_s / 3600,
                            unit.hold.required_s / 3600
                        )),
                        1,
                    ),
                    Form::empty(),
                ],
            ),
            Clause::new(
                " · ",
                vec![Form::new(text(qualification), 2), Form::empty()],
            ),
        ],
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
        .climb
        .units
        .iter()
        .find(|p| p.unit == UnitId::Ccd(id));
    let core_phase = scene
        .s
        .policy
        .climb
        .units
        .iter()
        .any(|p| matches!(p.unit, UnitId::Core(_)));
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
        None if core_phase => put(
            buf,
            r,
            0,
            1,
            &paint(Paint::Grey, "core tuning · physical CCD topology".into()),
            colours,
        ),
        None => put(
            buf,
            r,
            0,
            1,
            &paint(
                Paint::Grey,
                "unit state unavailable; no ordinary permission".into(),
            ),
            colours,
        ),
    }
    let cores: Vec<&CoreView> = scene.s.cores.iter().filter(|core| core.ccd == id).collect();
    let stride = if core_phase { 2 } else { 1 };
    let rows = usize::from(r.height.saturating_sub(3)) / stride;
    let mut shown = cores.len().min(rows);
    if cores.len() > shown && shown > 0 {
        shown -= 1;
        let more = paint(Paint::Grey, format!("+{} more", cores.len() - shown));
        put(buf, r, 0, r.height - 1, &more, colours);
    }
    for (i, core) in cores.iter().take(shown).enumerate() {
        let y = saturating_cells(i * stride + 3);
        let projection = if core_phase {
            scene
                .s
                .policy
                .climb
                .units
                .iter()
                .find(|unit| unit.unit == UnitId::Core(core.id))
        } else {
            projection
        };
        if core_phase {
            let line = projection.map_or_else(
                || {
                    paint(
                        Paint::Grey,
                        format!("core {} · unit state unavailable", core.id.get()),
                    )
                },
                |unit| {
                    let mut line =
                        Line::from(scene.paint(Paint::White, format!("core {} · ", core.id.get())));
                    line.spans
                        .extend(facts_line(scene, unit, width.saturating_sub(10)).spans);
                    line
                },
            );
            put(buf, r, 0, y, &line, colours);
        }
        let y = y + u16::from(core_phase);
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
