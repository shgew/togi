use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use togi_journal::CorePhase;
use togi_machine::{CcdId, ObservedOffset, TargetOffset};
use togi_tuner::{Limit, PhaseNo, Reached};

use super::at;
use crate::frames::colours::Paint;
use crate::frames::draw::{put, put_cells};
use crate::frames::fit::{Clause, fit_clauses, plain, wrap_styled};
use crate::frames::notes::CoreNote;
use crate::frames::scene::Scene;
use crate::model::{CoreState, CoreView, FailureSource, OffsetDecision};
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
        _ => None,
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

const RECORDS: usize = 8;
const LANE: usize = 50;
const GAUGE_X: usize = 4;
const VALUES_X: usize = 55;
const VALUES_WIDTH: usize = 22;

/// The number of lane cells a depth fills: depth 1 is offset -1.
fn depth(offset: TargetOffset) -> usize {
    usize::try_from(-offset.get()).unwrap_or(0).min(LANE)
}

fn gap(core: &CoreView) -> Option<i64> {
    core.solo.map(|solo| core.profile.get() - solo.get())
}

fn now_text(core: &CoreView) -> String {
    core.applied
        .map_or_else(|| "?".to_owned(), |observed| observed.0.to_string())
}

fn solo_text(core: &CoreView) -> String {
    core.solo
        .map_or_else(|| "?".to_owned(), |solo| solo.get().to_string())
}

fn gap_text(core: &CoreView) -> String {
    gap(core).map_or_else(|| "?".to_owned(), |gap| gap.to_string())
}

fn fail_text(core: &CoreView) -> String {
    core.fail
        .map_or_else(|| "none".to_owned(), |fail| fail.get().to_string())
}

fn failure_paint(core: &CoreView) -> Paint {
    if core.evidence.current_failure {
        Paint::Bad
    } else {
        Paint::Recorded
    }
}

/// The word for what the core does in the trial in flight, or at rest when none runs. A queued reset names an
/// otherwise idle core; a loaded one keeps its load word and shows the reset as a badge.
fn role_word(scene: &Scene<'_>, core: &CoreView) -> &'static str {
    let runs = scene.trial_runs();
    match core.state {
        CoreState::Suspect | CoreState::Member => "held failure",
        CoreState::Probe => "probing",
        CoreState::Parked => "parked",
        CoreState::Waiting
        | CoreState::Search
        | CoreState::Confirm
        | CoreState::Found
        | CoreState::AtLimit
        | CoreState::HasRoom => {
            if runs && core.judged {
                "judged"
            } else if runs && core.loaded {
                "loaded"
            } else if core.queued.is_some() {
                "reset queued"
            } else if core.applied == Some(ObservedOffset(0)) {
                "idle at 0"
            } else {
                "idle"
            }
        }
    }
}

fn parts(scene: &Scene<'_>, parts: Vec<(Paint, String)>) -> Line<'static> {
    Line::from(
        parts
            .into_iter()
            .map(|(paint, text)| scene.paint(paint, text))
            .collect::<Vec<_>>(),
    )
}

fn text_line(scene: &Scene<'_>, paint: Paint, text: impl Into<String>) -> Line<'static> {
    Line::from(scene.paint(paint, text.into()))
}

/// Draws CCD `ccd`'s overview into `rect`, exactly 77 cells wide and 26 rows tall: a heading row, a column-label
/// row, then eight three-row core records. Draws nowhere outside `rect`.
///
/// Columns: 0-2 id and badges, 3 the zero mark, 4-53 the lane (column `j` is depth `j + 1`), 54 spacer, 55-76
/// the values. Row A of a record is the id, the tuning profile bar and `tune`/`now`; row B the failure lane and
/// `s`olo, `g`ap, `f`ail; row C is blank under the lane and names the role.
pub(crate) fn draw_core_overview(buf: &mut Buffer, rect: Rect, scene: &Scene<'_>, ccd: CcdId) {
    put(
        buf,
        rect,
        at(GAUGE_X + 1),
        0,
        &text_line(scene, Paint::White, format!("CCD {}", ccd.get())),
    );
    put(
        buf,
        rect,
        at(GAUGE_X - 1),
        0,
        &text_line(scene, Paint::Grey, "0"),
    );
    for count in (10..=LANE).step_by(10) {
        put(
            buf,
            rect,
            at(GAUGE_X + count - 3),
            0,
            &text_line(scene, Paint::Grey, format!("-{count}")),
        );
    }
    put(
        buf,
        rect,
        0,
        1,
        &text_line(scene, Paint::Grey, "core  tune now"),
    );
    let legend = parts(
        scene,
        vec![
            (Paint::White, "█".to_owned()),
            (Paint::Grey, " tune  ".to_owned()),
            (Paint::Recorded, "█".to_owned()),
            (Paint::Grey, " fail  ! failing  R reset".to_owned()),
        ],
    );
    put(buf, rect, at(GAUGE_X + 12), 1, &legend);
    put(
        buf,
        rect,
        at(VALUES_X),
        1,
        &text_line(scene, Paint::Grey, "s=solo g=gap f=fail"),
    );
    for (record, core) in scene
        .s
        .cores
        .iter()
        .filter(|core| core.ccd == ccd)
        .take(RECORDS)
        .enumerate()
    {
        draw_record(buf, rect, scene, core, 2 + 3 * record);
    }
}

fn draw_record(buf: &mut Buffer, rect: Rect, scene: &Scene<'_>, core: &CoreView, top: usize) {
    let (a, b, c) = (at(top), at(top + 1), at(top + 2));
    let failing = core.evidence.current_failure;
    let queued = core.queued.is_some();
    let id_paint = if core.judged && scene.trial_runs() {
        Paint::White
    } else {
        Paint::Text
    };
    put(
        buf,
        rect,
        0,
        a,
        &text_line(scene, id_paint, format!("{:02}", core.id.get())),
    );
    if failing {
        put(buf, rect, 2, a, &text_line(scene, Paint::Bad, "!"));
        put(buf, rect, 0, b, &text_line(scene, Paint::Bad, "!"));
    } else if queued {
        put(buf, rect, 2, a, &text_line(scene, Paint::White, "R"));
    }
    if queued {
        put(buf, rect, 1, b, &text_line(scene, Paint::White, "R"));
    }
    let tuned = depth(core.profile);
    if tuned > 0 {
        put_cells(
            buf,
            rect,
            at(GAUGE_X),
            a,
            &text_line(scene, Paint::White, "█".repeat(tuned)),
        );
    }
    if let Some(fail) = core.fail {
        let at_depth = depth(fail);
        if at_depth > 0 {
            put_cells(
                buf,
                rect,
                at(GAUGE_X + at_depth - 1),
                b,
                &text_line(scene, failure_paint(core), "█"),
            );
        }
    }
    let tune_now = fit_clauses(
        VALUES_WIDTH,
        &[
            Clause::whole(
                "",
                parts(
                    scene,
                    vec![
                        (Paint::Grey, "tune ".to_owned()),
                        (Paint::White, format!("{:>3}", core.profile.get())),
                    ],
                ),
            ),
            Clause::whole(
                " ",
                parts(
                    scene,
                    vec![
                        (Paint::Grey, "now ".to_owned()),
                        (Paint::Text, format!("{:>3}", now_text(core))),
                    ],
                ),
            ),
        ],
    );
    put(buf, rect, at(VALUES_X), a, &tune_now);
    let labelled = |label: &str, value: String, paint: Paint| {
        parts(scene, vec![(Paint::Grey, label.to_owned()), (paint, value)])
    };
    let evidence = fit_clauses(
        VALUES_WIDTH,
        &[
            Clause::whole("", labelled("s", solo_text(core), Paint::Text)),
            Clause::whole(" ", labelled("g", gap_text(core), Paint::Text)),
            Clause::whole(
                " ",
                match core.fail {
                    Some(fail) => labelled("f", fail.get().to_string(), failure_paint(core)),
                    None => labelled("f ", "none".to_owned(), Paint::Text),
                },
            ),
        ],
    );
    put(buf, rect, at(VALUES_X), b, &evidence);
    put(
        buf,
        rect,
        at(VALUES_X),
        c,
        &text_line(scene, Paint::Text, role_word(scene, core)),
    );
}

/// The cores in CCD order, then core order.
fn in_ccd_order<'a>(scene: &Scene<'a>) -> Vec<&'a CoreView> {
    let mut cores: Vec<&CoreView> = scene.s.cores.iter().collect();
    cores.sort_by_key(|core| (core.ccd.get(), core.id));
    cores
}

/// What a core does, with the failure and reset flags, as the table's last column reads in full.
fn role_full(scene: &Scene<'_>, core: &CoreView) -> String {
    let mut words = vec![role_word(scene, core)];
    if core.evidence.current_failure {
        words.push("failing now");
    }
    if core.queued.is_some() && !words.contains(&"reset queued") {
        words.push("reset queued");
    }
    words.join(" · ")
}

/// The role column in `room` cells: every role word if they fit, else the `!` and `R` badges before the load
/// role, else the badges alone, so a current failure and a queued reset outlast the words.
fn role_cells(scene: &Scene<'_>, core: &CoreView, room: usize) -> Vec<(Paint, String)> {
    let full = role_full(scene, core);
    if full.chars().count() <= room {
        return vec![(Paint::Grey, full)];
    }
    let mut cells = Vec::new();
    if core.evidence.current_failure {
        cells.push((Paint::Bad, "!".to_owned()));
    }
    if core.queued.is_some() {
        cells.push((Paint::White, "R".to_owned()));
    }
    let role = role_word(scene, core);
    if cells.len() + 1 + role.chars().count() <= room {
        cells.push((Paint::Grey, format!(" {role}")));
    }
    cells
}

const TABLE_HEADINGS: [&str; 7] = ["CCD", "core", "tune", "now", "solo", "gap", "fail"];
const TABLE_WIDTHS: [usize; 7] = [3, 4, 4, 4, 4, 4, 4];
const TABLE_NUMBERS: usize = 3 + 4 * 6 + 6;

fn table_row(cells: &[String; 7]) -> String {
    cells
        .iter()
        .zip(TABLE_WIDTHS)
        .map(|(cell, width)| format!("{cell:>width$}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The compact numeric core table in `width` cells: one heading row, then one row per core in CCD and core order.
/// The numbers keep their columns; the role words go first when the width falls short, the badges last.
pub(crate) fn core_table(scene: &Scene<'_>, width: usize) -> Vec<Line<'static>> {
    let roles = width >= TABLE_NUMBERS + 2 + "reset queued".len();
    let mut heading = table_row(&TABLE_HEADINGS.map(str::to_owned));
    if roles {
        heading.push_str("  role");
    }
    let mut lines = vec![text_line(scene, Paint::Grey, heading)];
    for core in in_ccd_order(scene) {
        let numbers = |until: usize| {
            table_row(&[
                core.ccd.get().to_string(),
                format!("{:02}", core.id.get()),
                core.profile.get().to_string(),
                now_text(core),
                solo_text(core),
                gap_text(core),
                fail_text(core),
            ])
            .chars()
            .take(until)
            .collect::<String>()
        };
        let mut row = vec![(Paint::Text, numbers(usize::MAX))];
        if roles {
            row.push((Paint::Text, "  ".to_owned()));
            row.extend(role_cells(scene, core, width - TABLE_NUMBERS - 2));
        }
        lines.push(parts(scene, row));
    }
    lines
}

fn source_words(source: FailureSource) -> &'static str {
    match source {
        FailureSource::Alone => "alone",
        FailureSource::Named => "named",
        FailureSource::R7Charged => "R7-charged",
        FailureSource::Unrecorded => "source not recorded",
    }
}

fn decision_words(decision: &OffsetDecision) -> String {
    format!(
        "{} {} to {} while {}, journal #{}",
        decision.decision.as_str().replace('_', " "),
        decision.from.get(),
        decision.to.get(),
        decision.activity,
        decision.seq.get()
    )
}

fn gap_words(core: &CoreView) -> String {
    let solo = solo_text(core);
    match gap(core) {
        None => "solo ? · gap ?: no solo limit recorded yet".to_owned(),
        Some(0) => format!("solo {solo} · gap 0: at the solo limit"),
        Some(gap) if gap > 0 => {
            format!("solo {solo} · gap {gap}: {gap} counts shallower than solo")
        }
        Some(gap) => format!("solo {solo} · gap {gap}: {} counts deeper than solo", -gap),
    }
}

fn failure_words(core: &CoreView) -> String {
    let Some(fail) = core.fail else {
        return "no failure recorded".to_owned();
    };
    let head = if fail.get() == 0 {
        "failure at 0".to_owned()
    } else {
        format!("failure {}", fail.get())
    };
    match core.evidence.failure {
        Some(evidence) => format!(
            "{head} · {} · journal #{}",
            source_words(evidence.source),
            evidence.seq.get()
        ),
        None => format!("{head} · {}", source_words(FailureSource::Unrecorded)),
    }
}

fn room_words(scene: &Scene<'_>, core: &CoreView) -> Option<String> {
    let phase = scene.s.phases.map(|plan| (plan.phase, plan));
    match core.state {
        CoreState::HasRoom => Some(match phase {
            Some((PhaseNo::Concluded, _)) => "room: phase 2 has concluded".to_owned(),
            Some((PhaseNo::Two, plan)) if plan.candidates.iter().any(|c| c.core == core.id) => {
                "room: a phase 2 candidate".to_owned()
            }
            Some((PhaseNo::One | PhaseNo::Two, _)) | None => {
                "room: eligible to deepen in phase 2".to_owned()
            }
        }),
        CoreState::AtLimit => {
            Some("room: none, one count deeper reaches a recorded constraint or -50".to_owned())
        }
        CoreState::Waiting
        | CoreState::Search
        | CoreState::Confirm
        | CoreState::Found
        | CoreState::Suspect
        | CoreState::Member
        | CoreState::Probe
        | CoreState::Parked => None,
    }
}

fn load_words(scene: &Scene<'_>, core: &CoreView) -> String {
    let runs = scene.trial_runs();
    let load = match (runs && core.loaded, runs && core.judged) {
        (_, true) => "loaded and judged in this trial",
        (true, false) => "loaded, not judged in this trial",
        (false, false) => "not loaded",
    };
    format!("load: {load}")
}

fn park_words(core: &CoreView) -> Option<String> {
    match core.state {
        CoreState::Parked => Some(match core.returns_to {
            Some(back) => format!(
                "park: held shallower by the hunt, returns to {}",
                back.get()
            ),
            None => "park: held shallower by the hunt".to_owned(),
        }),
        CoreState::Suspect => Some("hunt: a candidate at its failing offset".to_owned()),
        CoreState::Member => {
            Some("hunt: a member of a kept group at its failing offset".to_owned())
        }
        CoreState::Probe => Some("hunt: the member probed shallower".to_owned()),
        CoreState::Waiting
        | CoreState::Search
        | CoreState::Confirm
        | CoreState::Found
        | CoreState::AtLimit
        | CoreState::HasRoom => None,
    }
}

/// The Cores page's body in `width` cells, unscrolled: one block per core with every exact value and where it
/// came from.
pub(crate) fn cores_page(scene: &Scene<'_>, width: usize) -> Vec<Line<'static>> {
    let cores = in_ccd_order(scene);
    if cores.is_empty() {
        return wrap_styled(
            "No cores yet: the journal records no session to show.",
            width,
            scene.style(Paint::Text),
        );
    }
    let mut lines = Vec::new();
    for (index, core) in cores.into_iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        let heading = format!(
            "Core {:02} · CCD {} · {}",
            core.id.get(),
            core.ccd.get(),
            state_word(core.state)
        );
        lines.extend(wrap_styled(&heading, width, scene.style(Paint::White)));
        let mut facts = vec![
            format!(
                "tune {} · now {}{}",
                core.profile.get(),
                now_text(core),
                match core.applied {
                    None => " (no readback recorded)",
                    Some(observed) if !(-50..=0).contains(&observed.0) => " (outside -50..0)",
                    Some(_) => "",
                }
            ),
            gap_words(core),
            failure_words(core),
        ];
        if core.evidence.current_failure {
            facts.push("! current failure: recorded in the step now running".to_owned());
        }
        facts.extend(room_words(scene, core));
        facts.push(load_words(scene, core));
        facts.extend(park_words(core));
        if let Some(seq) = core.queued {
            facts.push(format!(
                "R reset queued: command #{}, the core searches again at the next run",
                seq.get()
            ));
        }
        let note = plain(&note_line(scene, core, width.saturating_sub(2)));
        if !note.is_empty() && (core.queued.is_none() || note != "reset queued") {
            facts.push(note);
        }
        facts.push(match core.evidence.decision {
            Some(decision) => format!("last decision: {}", decision_words(&decision)),
            None => "last decision: none recorded".to_owned(),
        });
        for fact in facts {
            for mut line in wrap_styled(&fact, width.saturating_sub(2), scene.style(Paint::Text)) {
                line.spans.insert(0, Span::raw("  "));
                lines.push(line);
            }
        }
    }
    #[cfg(test)]
    crate::corpus::lines("page.cores", "", &lines);
    lines
}
