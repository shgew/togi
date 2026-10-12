//! The checking cycle as a checklist with its parts, and the combinations under it.

use std::collections::BTreeSet;
use std::time::Duration;

use ratatui::text::{Line, Span};
use togi_machine::{CoreId, Regime};
use togi_tuner::{CyclePart, CycleStep};

use super::super::colours::Paint;
use super::super::fit::{cut_words, rule};
use super::super::layout::SizeClass;
use super::super::measure::Tables;
use super::super::scene::{Scene, short_duration};
use super::super::terms::{first_open_part, part_name};
use super::table::{cell, number_ranges, stage_marker, table_row};
use crate::history::LOG_LIMIT;
use crate::model::CycleView;
use crate::words::{core_ids, kind_words, workload_display};

/// A cycle's rows and the index of the row it keeps in view: the running part, else the current step.
pub(super) struct CycleRows {
    /// The rows, rule first.
    pub(super) lines: Vec<Line<'static>>,
    /// The row of the running part, or of the current step; none when no step is open.
    pub(super) focus: Option<usize>,
}

fn seconds(n: u32) -> String {
    short_duration(Duration::from_secs(u64::from(n)))
}

/// The cycle within `height` rows: it leaves out the rows above the focus from the top, then the blank under
/// the rule. With no step open it keeps every row, but gives up the blank under the rule when it is too tall,
/// so a `+N more` under it does not count that blank.
pub(super) fn cycle_lines(
    scene: &Scene<'_>,
    cycle: &CycleView<'_>,
    t: &Tables,
    width: usize,
    class: SizeClass,
    height: usize,
) -> CycleRows {
    let plan = cycle.plan;
    let done = plan.steps.iter().filter(|step| step.done).count();
    let number = plan.number.map_or(0, std::num::NonZeroU32::get);
    let mut out = vec![
        rule(
            width,
            &cell(scene, Paint::Grey, format!("CYCLE {number}")),
            &cell(
                scene,
                Paint::Grey,
                format!("{} steps · {done} done", plan.steps.len()),
            ),
            scene.style(Paint::Track),
        ),
        Line::default(),
    ];
    let mut keep = vec![0];
    let mut focus = None;
    let schedule_shown = t.cycle[5].w > 0;
    if class != SizeClass::Compact {
        keep.push(out.len());
        out.push(table_row(
            &t.cycle,
            &[
                Line::default(),
                cell(scene, Paint::Grey, "#"),
                cell(scene, Paint::Grey, "kind"),
                cell(scene, Paint::Grey, "where"),
                cell(scene, Paint::Grey, "workload"),
                cell(scene, Paint::Grey, "schedule"),
                cell(scene, Paint::Grey, "result"),
            ],
        ));
    }
    let hunt_open = scene.s.hunt.is_some();
    for (i, step) in plan.steps.iter().enumerate() {
        let (mut marker, mut style, mut code) =
            (stage_marker(scene, "○"), Paint::Grey, Paint::Text);
        let mut result = Line::default();
        if step.done {
            (marker, style, code) = (stage_marker(scene, "■"), Paint::Text, Paint::White);
            let mut spans = vec![scene.paint(Paint::Good, "done")];
            let hunts = cycle.step_hunts.get(i).map_or(&[][..], Vec::as_slice);
            if !hunts.is_empty() {
                let word = if hunts.len() > 1 { "hunts " } else { "hunt " };
                spans.push(scene.paint(Paint::Grey, format!(" · {word}{}", number_ranges(hunts))));
            }
            result = Line::from(spans);
        } else if i == plan.current {
            (marker, style, code) = (stage_marker(scene, "►"), Paint::White, Paint::White);
            result = cell(scene, Paint::Running, step_position(scene, step));
            if plan.paused || hunt_open {
                marker = scene.paint(Paint::Hunt, "○");
                result = cell(scene, Paint::Hunt, "paused");
                if let Some(part) = first_open_part(step).filter(|_| step.parts.len() > 1) {
                    result = cell(scene, Paint::Hunt, format!("paused at part {part}"));
                }
            }
        }
        let (mut place, schedule) = step_schedule(step);
        if !schedule_shown {
            place.clone_from(&schedule);
        }
        let kind = Line::from(vec![
            scene.paint(code, step.regime.to_string()),
            scene.paint(style, format!(" {}", kind_words(step.regime))),
        ]);
        let current = i == plan.current && !step.done;
        if current {
            keep.push(out.len());
            focus = Some(out.len());
        }
        out.push(table_row(
            &t.cycle,
            &[
                Line::from(marker),
                cell(scene, style, format!("{:2}", i + 1)),
                kind,
                cell(scene, style, place),
                cell(scene, Paint::Grey, workload_display(step.workload)),
                cell(scene, style, schedule),
                result,
            ],
        ));
        if current {
            for (j, line) in part_lines(scene, t, step).into_iter().enumerate() {
                if step.parts.get(j).is_some_and(|part| part.running) && scene.trial_runs() {
                    focus = Some(out.len());
                }
                out.push(line);
            }
        }
    }
    let Some(at) = focus else {
        if out.len() > height {
            out.remove(1);
        }
        return CycleRows { lines: out, focus };
    };
    let excess = (at + 1).saturating_sub(height);
    if excess == 0 {
        return CycleRows { lines: out, focus };
    }
    let mut drop = BTreeSet::new();
    for i in 2..at {
        if drop.len() >= excess {
            break;
        }
        if !keep.contains(&i) {
            drop.insert(i);
        }
    }
    if drop.len() < excess {
        drop.insert(1);
    }
    let dropped = drop.len();
    let lines = out
        .into_iter()
        .enumerate()
        .filter_map(|(i, line)| (!drop.contains(&i)).then_some(line))
        .collect();
    CycleRows {
        lines,
        focus: Some(at - dropped),
    }
}

/// Where the running step is: its part, or its trial when it has one part.
fn step_position(scene: &Scene<'_>, step: &CycleStep) -> String {
    if step.parts.len() > 1
        && let Some(part) = first_open_part(step)
    {
        return format!("part {part} of {}", step.parts.len());
    }
    match scene.s.trial {
        Some(trial) if trial.count.of > 0 && scene.trial_runs() => {
            format!("trial {} of {}", trial.count.index, trial.count.of)
        }
        _ => String::new(),
    }
}

fn part_lines(scene: &Scene<'_>, t: &Tables, step: &CycleStep) -> Vec<Line<'static>> {
    if step.parts.len() == 1 && step.chains_complete {
        return Vec::new();
    }
    let more = !step.chains_complete;
    let mut out = Vec::new();
    let mut partials = false;
    for (j, part) in step.parts.iter().enumerate() {
        let (marker, style, faint) = if part.running && scene.trial_runs() {
            (
                Line::from(stage_marker(scene, "►")),
                Paint::White,
                Paint::White,
            )
        } else {
            (Line::default(), Paint::Text, Paint::Grey)
        };
        let tree = if j + 1 == step.parts.len() && !more {
            "└"
        } else {
            "├"
        };
        partials = partials || !part.full;
        out.push(table_row(
            &t.cycle,
            &[
                marker,
                Line::default(),
                Line::from(vec![
                    scene.paint(Paint::Track, tree),
                    scene.paint(Paint::Text, " "),
                    scene.paint(style, part_name(step.regime, part)),
                ]),
                cell(scene, style, core_ids(part.cores.iter())),
                Line::default(),
                cell(scene, faint, part_schedule(part)),
                part_result(scene, part),
            ],
        ));
    }
    if more {
        let pending = if partials {
            "more partials may follow"
        } else {
            "partials may follow"
        };
        out.push(table_row(
            &t.cycle,
            &[
                Line::default(),
                Line::default(),
                Line::from(vec![
                    scene.paint(Paint::Track, "└"),
                    scene.paint(Paint::Text, " "),
                    scene.paint(Paint::Grey, pending),
                ]),
            ],
        ));
    }
    out
}

fn part_result(scene: &Scene<'_>, part: &CyclePart) -> Line<'static> {
    let trials = scene.s.trial.filter(|trial| trial.count.of > 0);
    if let (true, Some(trial)) = (part.running && scene.trial_runs(), trials) {
        return cell(
            scene,
            Paint::Running,
            format!("trial {} of {}", trial.count.index, trial.count.of),
        );
    }
    if part.passed + part.failed == 0 {
        return Line::default();
    }
    let total = part.short + part.long;
    let passed = part.passed.min(total);
    if part.failed > 0 {
        cell(scene, Paint::Bad, format!("{} failed", part.failed))
    } else if part.done {
        cell(scene, Paint::Good, format!("{passed}/{total} passed"))
    } else {
        cell(scene, Paint::Text, format!("{passed}/{total} passed"))
    }
}

fn part_schedule(part: &CyclePart) -> String {
    let mut out = Vec::new();
    for (count, length) in [(part.short, part.short_s), (part.long, part.long_s)] {
        match count {
            0 => {}
            1 => out.push(seconds(length)),
            _ => out.push(format!("{count} x {}", seconds(length))),
        }
    }
    out.join(" + ")
}

/// Where a step loads and how long it runs, in the two columns that say it.
fn step_schedule(step: &CycleStep) -> (String, String) {
    if step.regime == Regime::R7 {
        let mut parts = step.parts.len().to_string();
        if !step.chains_complete {
            parts.push('+');
        }
        let ccds = step
            .parts
            .iter()
            .filter_map(|part| part.ccd)
            .collect::<BTreeSet<_>>()
            .len();
        return (
            format!("{ccds} CCDs · {parts} parts"),
            format!("{parts} parts"),
        );
    }
    let place = if step.regime == Regime::R6 {
        "all cores idle"
    } else {
        "one core at a time"
    };
    match step.parts.as_slice() {
        [part] => (place.to_owned(), part_schedule(part)),
        [first, ..] => (
            place.to_owned(),
            format!("{} x {}", step.parts.len(), seconds(first.short_s)),
        ),
        [] => (place.to_owned(), String::new()),
    }
}

/// The recorded combinations: a column per member core, a row per combination, and the profile now under them.
pub(super) fn combination_lines(
    scene: &Scene<'_>,
    t: &Tables,
    width: usize,
    class: SizeClass,
) -> Vec<Line<'static>> {
    let right = if class == SizeClass::Wide {
        "offsets at which these cores failed together"
    } else {
        "failed together"
    };
    let mut out = vec![
        rule(
            width,
            &cell(scene, Paint::Grey, "COMBINATIONS"),
            &cell(scene, Paint::Grey, right),
            scene.style(Paint::Track),
        ),
        Line::default(),
    ];
    let ids: Vec<CoreId> = scene
        .s
        .combos
        .iter()
        .flat_map(|combo| combo.members.iter().map(|member| member.core))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let shown = t.combos[1]
        .w
        .checked_div(t.member)
        .map_or(0, |fit| ids.len().min(fit));
    let offsets = |value: &dyn Fn(CoreId) -> (String, Paint)| -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, &id) in ids[..shown].iter().enumerate() {
            let (text, paint) = value(id);
            if i > 0 {
                spans.push(Span::raw(" ".repeat(t.member.saturating_sub(3))));
            }
            spans.push(scene.paint(paint, format!("{text:>3}")));
        }
        Line::from(spans)
    };
    let found = if t.combos[2].w < "found by".len() {
        "hunt"
    } else {
        "found by"
    };
    if class != SizeClass::Compact {
        out.push(table_row(
            &t.combos,
            &[
                Line::default(),
                offsets(&|id| (format!("{:02}", id.get()), Paint::Grey)),
                cell(scene, Paint::Grey, found),
                cell(scene, Paint::Grey, "against the profile now"),
            ],
        ));
    }
    if 0 < shown && shown < ids.len() {
        let text = format!(
            "columns show {shown} of {} member cores; not shown: {} · l: last {LOG_LIMIT} events",
            ids.len(),
            core_ids(ids[shown..].iter().copied()),
        );
        out.push(cell(scene, Paint::Grey, cut_words(&text, width)));
    }
    for combo in scene.s.combos {
        let member = |id: CoreId| -> (String, Paint) {
            let Some(found) = combo.members.iter().find(|member| member.core == id) else {
                return (String::new(), Paint::Grey);
            };
            let paint = if combo.holds.is_some_and(|holds| holds.core == id) {
                Paint::Combination
            } else if combo.clear.contains(id) {
                Paint::Text
            } else {
                Paint::Grey
            };
            (found.offset.get().to_string(), paint)
        };
        let how = if combo.probed { "probed" } else { "unresolved" };
        let mut by = vec![scene.paint(Paint::Text, format!("hunt {}", combo.hunt))];
        if t.combos[2].w >= 18 {
            by.push(scene.paint(Paint::Grey, format!(", {how}")));
        }
        let against = match combo.holds {
            Some(holds) => cell(
                scene,
                Paint::Combination,
                format!(
                    "holds core {:02} at {}",
                    holds.core.get(),
                    holds.offset.get()
                ),
            ),
            None if !combo.clear.is_empty() => Line::from(vec![
                scene.paint(Paint::Grey, "clear: "),
                scene.paint(
                    Paint::Text,
                    format!("{} shallower", core_ids(combo.clear.iter())),
                ),
            ]),
            None => Line::default(),
        };
        out.push(table_row(
            &t.combos,
            &[
                cell(scene, Paint::Combination, combo.id.to_string()),
                offsets(&member),
                Line::from(by),
                against,
            ],
        ));
    }
    if shown > 0 {
        let now = offsets(&|id| {
            let text = scene
                .core(id)
                .map_or_else(String::new, |core| core.profile.get().to_string());
            (text, Paint::White)
        });
        out.push(Line::default());
        out.push(table_row(
            &t.combos,
            &[cell(scene, Paint::White, "now"), now],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;
    use togi_machine::Regime;
    use togi_tuner::CyclePlan;

    use super::super::super::layout::SizeClass;
    use super::super::super::measure::Tables;
    use super::super::fixtures::{
        at, core_view, cycle_part, cycle_plan, cycle_step, hunt_facts, hunt_plan, scene, texts,
        trial_view,
    };
    use super::{combination_lines, cycle_lines};
    use crate::model::{ComboView, CycleView, HuntView, Model, SessionView, StopView};
    use crate::tests::kit::{core, nz, off, set};
    use togi_journal::{Member, ShutdownReason};
    use togi_tuner::CombinationId;

    fn plan() -> CyclePlan {
        cycle_plan(
            vec![
                cycle_step(
                    Regime::R1,
                    vec![cycle_part(&[0, 1, 2], None, true, false, true)],
                    true,
                    true,
                ),
                cycle_step(
                    Regime::R3,
                    vec![
                        cycle_part(&[0], None, true, false, true),
                        cycle_part(&[1], None, true, false, true),
                        cycle_part(&[2], None, true, true, false),
                        cycle_part(&[3], None, true, false, false),
                    ],
                    false,
                    true,
                ),
                cycle_step(
                    Regime::R7,
                    vec![
                        cycle_part(&[0, 1], Some(0), true, false, false),
                        cycle_part(&[2, 3], Some(1), true, false, false),
                    ],
                    false,
                    false,
                ),
            ],
            1,
            false,
        )
    }

    fn cycle_rows(
        plan: &CyclePlan,
        hunts: &[Vec<u32>],
        height: usize,
        hunting: bool,
    ) -> (Vec<String>, Option<usize>) {
        let model = Model::new();
        let trial = trial_view(3, 5);
        let hunt_plan = hunt_plan(Vec::new(), Vec::new());
        let facts = hunt_facts();
        let mut s = model.snapshot();
        s.trial = Some(&trial);
        if hunting {
            s.hunt = Some(HuntView {
                plan: &hunt_plan,
                facts: &facts,
            });
        }
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let cycle = CycleView {
            plan,
            step_hunts: hunts,
            resume: None,
        };
        let drawn = cycle_lines(
            &scene,
            &cycle,
            &Tables::new(SizeClass::Wide, 120),
            120,
            SizeClass::Wide,
            height,
        );
        (texts(&drawn.lines), drawn.focus)
    }

    #[test]
    fn the_cycle_lists_its_steps_and_the_running_part() {
        let (rows, focus) = cycle_rows(&plan(), &[vec![1, 2, 3, 7], Vec::new()], 40, false);
        assert!(rows[0].starts_with("CYCLE 2") && rows[0].ends_with("3 steps · 1 done"));
        assert!(rows[2].contains("kind") && rows[2].contains("schedule"));
        assert!(rows[3].contains("R1 light") && rows[3].contains("done · hunts 1-3, 7"));
        assert!(rows[4].contains("R3 load steps") && rows[4].contains("part 3 of 4"));
        assert!(rows[4].starts_with('►'));
        assert!(rows[5].contains("├ core 00") && rows[5].contains("1/1 passed"));
        assert!(
            rows[7].starts_with('►') && !rows[7].contains('└') && rows[7].contains("├ core 02")
        );
        assert!(rows[7].contains("trial 3 of 5"));
        assert!(rows[8].contains("└ core 03"));
        assert!(rows[9].starts_with('○') && rows[9].contains("2 CCDs · 2+ parts"));
        assert_eq!(focus, Some(7));
    }

    #[test]
    fn a_hunt_pauses_the_running_step_at_its_part() {
        let (rows, _) = cycle_rows(&plan(), &[], 40, true);
        assert!(
            rows[4].starts_with('○') && rows[4].contains("paused at part 3"),
            "{}",
            rows[4]
        );
        let mut paused = plan();
        paused.steps[1].parts.truncate(1);
        paused.paused = true;
        let (rows, _) = cycle_rows(&paused, &[], 40, false);
        assert!(rows[4].ends_with("paused"), "{}", rows[4]);
    }

    #[test]
    fn a_short_panel_leaves_out_the_rows_above_the_running_part() {
        let (full, at) = cycle_rows(&plan(), &[], 40, false);
        assert_eq!(at, Some(7));
        let (short, focus) = cycle_rows(&plan(), &[], 6, false);
        let focus = focus.expect("a running part");
        assert!(short[focus].contains("core 02"), "{short:?}");
        assert!(short.len() < full.len());
        assert!(short[0].starts_with("CYCLE 2"));
        assert!(short[2].contains("kind"), "the header stays: {short:?}");
        assert!(focus < 6, "{short:?}");
    }

    #[test]
    fn a_step_with_chains_to_come_says_partials_may_follow() {
        let mut open = plan();
        open.current = 2;
        let (rows, _) = cycle_rows(&open, &[], 40, false);
        assert!(
            rows.last()
                .is_some_and(|row| row.contains("└ partials may follow")),
            "{rows:?}"
        );
        open.steps[2].parts[0].full = false;
        let (rows, _) = cycle_rows(&open, &[], 40, false);
        assert!(
            rows.last()
                .is_some_and(|row| row.contains("└ more partials may follow"))
        );
        assert!(rows.iter().any(|row| row.contains("partial CCD 0")));
    }

    fn combos(members: &[u8]) -> Vec<ComboView> {
        vec![ComboView {
            id: CombinationId::new(nz(1)),
            members: members
                .iter()
                .rev()
                .map(|&id| Member {
                    core: core(id),
                    offset: off(-25),
                })
                .collect(),
            hunt: 7,
            probed: true,
            fallback: false,
            holds: Some(Member {
                core: core(members[0]),
                offset: off(-25),
            }),
            clear: set(&members[1..2]),
        }]
    }

    fn combination_rows(members: &[u8], class: SizeClass) -> Vec<String> {
        combination_rows_of(&combos(members), class)
    }

    fn combination_rows_of(combos: &[ComboView], class: SizeClass) -> Vec<String> {
        let cores: Vec<_> = (0..16)
            .map(|id| {
                let mut view = core_view(id, id / 8);
                view.profile = off(-20);
                view
            })
            .collect();
        let model = Model::new();
        let mut s = model.snapshot();
        s.cores = &cores;
        s.combos = combos;
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        texts(&combination_lines(
            &scene,
            &Tables::new(class, 120),
            120,
            class,
        ))
    }

    #[test]
    fn combination_overflow_names_the_omitted_cores_and_the_journal_bound() {
        for (members, omitted) in [
            (vec![0, 1, 2, 3, 4, 5, 6, 7, 9], "09"),
            (vec![0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 15], "09 11 15"),
        ] {
            let rows = combination_rows(&members, SizeClass::Wide);
            assert_eq!(
                rows[3],
                format!(
                    "columns show 8 of {} member cores; not shown: {omitted} · l: last 400 events",
                    members.len()
                )
            );
            for &id in &members[..8] {
                assert!(rows[2].contains(&format!("{id:02}")), "{}", rows[2]);
            }
            for &id in &members[8..] {
                assert!(!rows[2].contains(&format!("{id:02}")), "{}", rows[2]);
            }
        }
    }

    #[test]
    fn a_combination_row_says_what_it_holds_and_the_profile_now() {
        let rows = combination_rows(&[0, 2], SizeClass::Wide);
        assert!(rows[0].starts_with("COMBINATIONS"));
        assert!(rows[2].contains("found by") && rows[2].contains("against the profile now"));
        assert!(rows[3].starts_with("C1") && rows[3].contains("hunt 7, probed"));
        assert!(rows[3].contains("holds core 00 at -25"));
        let now = rows.last().expect("the profile now");
        assert!(now.starts_with("now") && now.contains("-20"), "{rows:?}");
        let compact = combination_rows(&[0, 2], SizeClass::Compact);
        assert_eq!(compact.len(), 3, "{compact:?}");
        assert!(compact[2].starts_with("C1"));
    }

    #[test]
    fn a_step_row_names_no_trial_once_the_session_has_stopped() {
        let single = cycle_plan(
            vec![cycle_step(
                Regime::R6,
                vec![cycle_part(&[0], None, true, true, false)],
                false,
                true,
            )],
            0,
            false,
        );
        let model = Model::new();
        let trial = trial_view(1, 1);
        let session = SessionView {
            start: at(0),
            activity: None,
        };
        let stopped = StopView {
            at: at(1),
            reason: ShutdownReason::Signal,
            saved: false,
        };
        let step_row = |stop: Option<&StopView>| {
            let mut s = model.snapshot();
            s.session = Some(&session);
            s.trial = Some(&trial);
            s.stopped = stop;
            let zone = TimeZone::UTC;
            let scene = scene(&s, &zone);
            let cycle = CycleView {
                plan: &single,
                step_hunts: &[],
                resume: None,
            };
            let drawn = cycle_lines(
                &scene,
                &cycle,
                &Tables::new(SizeClass::Wide, 120),
                120,
                SizeClass::Wide,
                40,
            );
            texts(&drawn.lines)[3].clone()
        };
        assert!(step_row(None).ends_with("trial 1 of 1"));
        let row = step_row(Some(&stopped));
        assert!(row.contains("R6") && !row.contains("trial"), "{row}");
    }

    #[test]
    fn a_part_says_how_many_trials_failed_or_have_passed_so_far() {
        let mut failing = cycle_part(&[0], None, true, false, false);
        failing.failed = 1;
        let mut partway = cycle_part(&[1], None, true, false, false);
        partway.short = 2;
        partway.passed = 1;
        let plan = cycle_plan(
            vec![cycle_step(Regime::R3, vec![failing, partway], false, true)],
            0,
            false,
        );
        let (rows, _) = cycle_rows(&plan, &[], 40, false);
        let first = rows
            .iter()
            .find(|row| row.contains("core 00"))
            .expect("part 1");
        let second = rows
            .iter()
            .find(|row| row.contains("core 01"))
            .expect("part 2");
        assert!(first.ends_with("1 failed"), "{rows:?}");
        assert!(second.ends_with("1/2 passed"), "{rows:?}");
    }

    #[test]
    fn a_combination_without_a_member_leaves_its_cell_blank_and_says_what_is_clear() {
        let mut all = combos(&[0, 2]);
        all.push(ComboView {
            id: CombinationId::new(nz(2)),
            members: vec![
                Member {
                    core: core(2),
                    offset: off(-25),
                },
                Member {
                    core: core(3),
                    offset: off(-25),
                },
            ],
            hunt: 8,
            probed: false,
            fallback: false,
            holds: None,
            clear: set(&[2]),
        });
        let rows = combination_rows_of(&all, SizeClass::Wide);
        let header = &rows[2];
        let end = |id: &str| header.find(id).expect("a member column") + id.len();
        let second = &rows[4];
        assert!(second.starts_with("C2"), "{rows:?}");
        assert_eq!(&second[end("00") - 3..end("00")], "   ", "{rows:?}");
        assert_eq!(&second[end("02") - 3..end("02")], "-25", "{rows:?}");
        assert_eq!(&second[end("03") - 3..end("03")], "-25", "{rows:?}");
        assert!(second.ends_with("clear: 02 shallower"), "{rows:?}");
    }
}
