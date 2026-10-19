use ratatui::buffer::Buffer;
use ratatui::text::{Line, Span};
use togi_journal::CorePhase;
use togi_machine::{CcdId, TargetOffset};
use togi_tuner::{Limit, Reached};

use crate::frames::colours::Paint;
use crate::frames::draw::{put, put_cells};
use crate::frames::fit::{Clause, Form, cut_words, fit_clauses, tint};
use crate::frames::measure::Ccd;
use crate::frames::notes::{CoreNote, top_note};
use crate::frames::scene::Scene;
use crate::model::{CoreState, CoreView};
use crate::numbers::saturating_cells;
use crate::screen::Colours;
use crate::words::phase_word;

/// The word a core's state is shown as.
pub(super) fn state_word(state: CoreState) -> &'static str {
    match state {
        CoreState::Waiting => "WAITING",
        CoreState::Search => phase_word(CorePhase::Search),
        CoreState::Confirm => "CONFIRM",
        CoreState::Found => "FOUND",
        CoreState::AtLimit => phase_word(CorePhase::AtLimit),
        CoreState::HasRoom => phase_word(CorePhase::HasRoom),
        CoreState::Suspect => "SUSPECT",
        CoreState::Member => "MEMBER",
        CoreState::Probe => "PROBE",
        CoreState::Parked => "PARKED",
    }
}

fn combination(core: &CoreView) -> Option<u32> {
    match core.holder {
        Some(Limit::Reached(Reached::Combination(id))) => Some(id.get()),
        Some(Limit::Reached(Reached::FailurePoint { .. }) | Limit::Floor) | None => None,
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

/// The glyphs a gauge draws applied depth, failure point and group failure with. The help legend and examples
/// use the same set.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Marks {
    pub(crate) applied: char,
    pub(crate) failure: char,
    pub(crate) group: char,
}

/// The marks of a colour profile: the console's half-height bars, or the rich profile's three-quarter-height bar
/// for the applied depth and the failure point (told apart by colour) and a dashed red line for group failures.
pub(crate) const fn marks(colours: Colours) -> Marks {
    match colours {
        Colours::Rich => Marks {
            applied: '▆',
            failure: '▆',
            group: '┆',
        },
        Colours::Full | Colours::Console | Colours::None => Marks {
            applied: '▄',
            failure: '▄',
            group: '▀',
        },
    }
}

/// The gauge of a core in `cells` cells, 2 counts to a cell at 25 cells and 1 at 50; `applied` is the offset it
/// draws as applied.
pub(super) fn gauge(
    scene: &Scene<'_>,
    core: &CoreView,
    applied: Option<i64>,
    cells: usize,
) -> Line<'static> {
    if cells == 0 {
        return Line::default();
    }
    let back = [core.solo, core.pass, core.returns_to]
        .into_iter()
        .flatten()
        .map(TargetOffset::get)
        .fold(0, i64::min);
    let (held_from, held_to) = if combination(core).is_some() {
        (
            1 - core.profile.get(),
            core.solo.map_or(-back, |solo| -solo.get()),
        )
    } else {
        (1, 0)
    };
    let marks = marks(scene.colours);
    let total = i64::from(saturating_cells(cells));
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_glyph = ' ';
    let mut run_paint = None;
    for i in 0..total {
        let (lo, hi) = (i * 50 / total + 1, (i + 1) * 50 / total);
        let hits = |point: i64| (lo..=hi).contains(&-point);
        let (glyph, paint) = if core.fail.is_some_and(|fail| hits(fail.get())) {
            (marks.failure, Some(Paint::Bad))
        } else if core.group_fails.iter().any(|mark| hits(mark.get())) {
            (marks.group, Some(Paint::Bad))
        } else if applied.is_some_and(|applied| -applied >= lo) {
            (
                marks.applied,
                Some(if core.judged {
                    Paint::White
                } else {
                    Paint::Grey
                }),
            )
        } else if lo.max(held_from) <= hi.min(held_to) {
            ('░', Some(Paint::Combination))
        } else if -back >= lo {
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

/// The note at the end of `core`'s row in `width` cells.
pub(crate) fn note_line(scene: &Scene<'_>, core: &CoreView, width: usize) -> Line<'static> {
    let note = if core.state == CoreState::Probe && !core.group_fails.is_empty() {
        CoreNote::GroupFailed(&core.group_fails)
    } else if let Some(back) = core.returns_to {
        CoreNote::ReturnsTo(back)
    } else if let Some(confirm) = core.confirm {
        CoreNote::Confirming {
            offset: confirm.offset,
            light: confirm.light,
            heavy: confirm.heavy,
            needed: confirm.needed,
        }
    } else if let Some(combination) = combination(core) {
        CoreNote::HeldBy {
            combination,
            solo: core.solo,
        }
    } else if let Some(after) = core.gave_back {
        CoreNote::BackedOff {
            after: after.get(),
            solo: core.solo,
        }
    } else if core.state == CoreState::Found && core.solo.is_some() {
        CoreNote::Found(core.solo.unwrap_or(core.profile))
    } else if core.state == CoreState::Search {
        CoreNote::Searching {
            passed: core.pass,
            next: core.next,
        }
    } else if core.queued.is_some() {
        CoreNote::Queued("reset queued")
    } else {
        return Line::default();
    };
    let line = note.line(width, scene.colours);
    #[cfg(test)]
    crate::corpus::line("dash.note", "", &line);
    line
}

/// What this trial does with a CCD's cores, in full and in a word.
pub(super) fn ccd_role(scene: &Scene<'_>, id: CcdId) -> (String, String) {
    let role = role(scene, id);
    #[cfg(test)]
    {
        crate::corpus::text("dash.role", "full", &role.0);
        crate::corpus::text("dash.role", "short", &role.1);
    }
    role
}

fn role(scene: &Scene<'_>, id: CcdId) -> (String, String) {
    let (mut loaded, mut judged, mut parked, mut total) = (0, 0, 0, 0);
    let (mut suspects, mut members, mut probes) = (0, 0, 0);
    let (mut at_zero, mut parked_zero) = (true, true);
    let runs = scene.trial_runs();
    for core in scene.s.cores.iter().filter(|core| core.ccd == id) {
        total += 1;
        let zero = shown_offset(scene, core) == Some(0);
        at_zero &= zero;
        loaded += usize::from(core.loaded && runs);
        judged += usize::from(core.judged && runs);
        match core.state {
            CoreState::Parked => {
                parked += 1;
                parked_zero &= zero;
            }
            CoreState::Suspect => suspects += 1,
            CoreState::Member => members += 1,
            CoreState::Probe => {
                members += 1;
                probes += 1;
            }
            CoreState::Waiting
            | CoreState::Search
            | CoreState::Confirm
            | CoreState::Found
            | CoreState::AtLimit
            | CoreState::HasRoom => {}
        }
    }
    let parked_words = if parked_zero { "parked at 0" } else { "parked" };
    let pair = |full: String, short: &str| (full, short.to_owned());
    match () {
        () if total == 0 => (String::new(), String::new()),
        () if parked == total && loaded == total => {
            pair(format!("{parked_words}, still under load"), "parked")
        }
        () if parked == total && loaded > 0 => {
            pair(format!("{parked_words}, {loaded} under load"), "parked")
        }
        () if parked == total => pair(format!("{parked_words}, idle"), "parked"),
        () if loaded == total && suspects == total => {
            pair("suspects at their failing offsets".into(), "suspects")
        }
        () if loaded == total && members == total && probes > 0 => pair(
            "members, one probed shallower than its failing offset".into(),
            "members",
        ),
        () if loaded == total && members == total => {
            pair("members at their failing offsets".into(), "members")
        }
        () if loaded == total => pair("all under load".into(), "all loaded"),
        () if judged == total && loaded == 0 => pair("all idle, all judged".into(), "judged"),
        () if loaded == 0 && at_zero => pair("waiting at 0".into(), "waiting"),
        () if loaded == 0 => pair("not under load".into(), "idle"),
        () if loaded == 1 => pair("one core under load at a time".into(), "one loaded"),
        () => (
            format!("{loaded} under load · {judged} judged"),
            format!("{loaded} loaded"),
        ),
    }
}

/// Draws one CCD's core table.
pub(crate) fn draw_ccd(buf: &mut Buffer, ccd: &Ccd, scene: &Scene<'_>) {
    let (r, col, id) = (ccd.rect, ccd.columns, ccd.id);
    let width = usize::from(r.width);
    let colours = scene.colours;
    let paint = |p: Paint, text: String| Line::from(scene.paint(p, text));
    let (role, short) = ccd_role(scene, id);
    let name = format!("CCD {}", id.get());
    if col.brief {
        let room = col.gauge.saturating_sub(name.len() + 3);
        let head = Line::from(vec![
            scene.paint(Paint::White, name),
            Span::raw(" "),
            scene.paint(Paint::Grey, cut_words(&short, room, colours)),
        ]);
        put(buf, r, 0, 0, &head, colours);
    } else {
        put(buf, r, 0, 0, &paint(Paint::White, name), colours);
    }
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
    let cores: Vec<&CoreView> = scene.s.cores.iter().filter(|core| core.ccd == id).collect();
    if col.brief {
        put(
            buf,
            r,
            saturating_cells(col.failure),
            0,
            &paint(Paint::Grey, "fail".into()),
            colours,
        );
        if cores.iter().any(|core| core.returns_to.is_some()) {
            put(
                buf,
                r,
                saturating_cells(col.note),
                0,
                &paint(Paint::Grey, "back".into()),
                colours,
            );
        }
    } else {
        put(
            buf,
            r,
            saturating_cells(col.failure.saturating_sub(1)),
            0,
            &paint(Paint::Grey, "fail pt".into()),
            colours,
        );
        let both = fit_clauses(
            width.saturating_sub(col.note),
            &[Clause::new(
                "",
                vec![Form::new(role, 1), Form::new(short, 0)],
            )],
            colours,
        );
        put(
            buf,
            r,
            saturating_cells(col.note),
            0,
            &tint(both, scene.style(Paint::Grey)),
            colours,
        );
    }
    let rows = usize::from(r.height.saturating_sub(1));
    let mut shown = cores.len().min(rows);
    if cores.len() > shown && shown > 0 {
        shown -= 1;
        let more = paint(Paint::Grey, format!("+{} more", cores.len() - shown));
        put(buf, r, 0, r.height - 1, &more, colours);
    }
    let top = (col.note < width).then(|| scene.s.r7_top());
    for (i, core) in cores.iter().take(shown).enumerate() {
        let y = saturating_cells(i + 1);
        let applied = shown_offset(scene, core);
        let own = if core.judged {
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
        let state = match core.state {
            CoreState::Parked | CoreState::Waiting => Paint::Grey,
            CoreState::Probe => Paint::Hunt,
            CoreState::Search
            | CoreState::Confirm
            | CoreState::Found
            | CoreState::AtLimit
            | CoreState::HasRoom
            | CoreState::Suspect
            | CoreState::Member => Paint::Text,
        };
        put(
            buf,
            r,
            saturating_cells(col.state),
            y,
            &paint(state, state_word(core.state).into()),
            colours,
        );
        put_cells(
            buf,
            r,
            saturating_cells(col.gauge),
            y,
            &gauge(scene, core, applied, col.cells),
            colours,
        );
        if let Some(fail) = core.fail {
            let fail_paint = if core.judged {
                Paint::Text
            } else {
                Paint::Grey
            };
            put(
                buf,
                r,
                saturating_cells(col.failure),
                y,
                &paint(fail_paint, format!("{:>1$}", fail.get(), col.failure_width)),
                colours,
            );
        }
        if let Some(top) = &top {
            let room = width - col.note;
            let mut note = note_line(scene, core, room);
            if let Some(by_offset) = top.get(core.id).flatten() {
                note = top_note(&note, by_offset, room, colours);
            }
            put(buf, r, saturating_cells(col.note), y, &note, colours);
        }
    }
}
