//! The context panel: the BIOS profile with the panel under it.

use ratatui::text::Line;
use togi_journal::Activity;

use super::super::dash::r7_lines;
use super::super::draw::bounded_rows;
use super::super::layout::SizeClass;
use super::super::measure::Measure;
use super::super::scene::Scene;
use super::cycle::{combination_lines, cycle_lines};
use super::hunt::hunt_lines;
use super::search::{bios_lines, phase2_lines, turn_lines};
use super::table::is_blank;

/// The BIOS profile with the panel under it: a hunt, the search turns, or the activity. The profile is always
/// shown; the panel under it keeps the rows it leaves.
pub(super) fn context(scene: &Scene<'_>, measure: &Measure) -> Vec<Line<'static>> {
    let class = measure.layout.class;
    let width = usize::from(measure.context.width);
    let height = usize::from(measure.context.height);
    let panel = if let Some(hunt) = &scene.s.hunt {
        Some(hunt_lines(scene, hunt, &measure.tables, width, class))
    } else if scene.s.turns.is_empty() {
        None
    } else {
        Some(turn_lines(scene, &measure.tables, width, class))
    };
    let mut out = bios_lines(scene, width, class);
    if out.is_empty() {
        return panel.unwrap_or_else(|| activity(scene, measure));
    }
    let room = height.saturating_sub(out.len() + 1);
    out.push(Line::default());
    if let Some(mut panel) = panel {
        if panel.len() > room && class == SizeClass::Compact {
            panel.retain(|row| !is_blank(row));
        }
        out.extend(bounded_rows(panel, room, scene.colours));
    } else {
        let mut below = measure.clone();
        below.context.height = u16::try_from(room).unwrap_or(u16::MAX);
        out.extend(activity(scene, &below));
    }
    out
}

fn activity(scene: &Scene<'_>, measure: &Measure) -> Vec<Line<'static>> {
    let r7 = r7_lines(scene, measure);
    activity_under(scene, measure, &r7)
}

/// The panel below the BIOS profile in the context rectangle's rows: the open round, or the cycle, the
/// combinations and the R7 lines `r7`.
pub(super) fn activity_under(
    scene: &Scene<'_>,
    measure: &Measure,
    r7: &[Line<'static>],
) -> Vec<Line<'static>> {
    let width = usize::from(measure.context.width);
    let height = usize::from(measure.context.height);
    let class = measure.layout.class;
    let t = &measure.tables;
    let deepening = scene
        .s
        .session
        .is_some_and(|session| session.activity == Some(Activity::Deepening));
    let open = scene
        .s
        .phases
        .is_some_and(|phases| phases.round.is_some_and(|round| round > 0));
    if deepening && open {
        return round(scene, width, height, r7);
    }
    let room = if r7.is_empty() {
        height
    } else {
        height.saturating_sub(r7.len() + 1)
    };
    let build = |rows: usize| {
        let mut out = Vec::new();
        let mut focus = None;
        if let Some(cycle) = &scene.s.cycle {
            let cycle_rows = cycle_lines(scene, cycle, t, width, class, rows);
            out = cycle_rows.lines;
            focus = cycle_rows.focus;
        }
        if !scene.s.combos.is_empty() {
            if !out.is_empty() {
                out.push(Line::default());
            }
            out.extend(combination_lines(scene, t, width, class));
        }
        (out, focus)
    };
    let (mut out, _) = build(room);
    if r7.is_empty() {
        return out;
    }
    if out.len() > room {
        let (bounded, focus) = build(room.saturating_sub(1));
        if room > 0 && focus.is_none_or(|focus| focus + 1 < room) {
            out = bounded_rows(bounded, room, scene.colours);
        } else {
            out.truncate(room);
        }
    }
    if !out.is_empty() {
        out.push(Line::default());
    }
    out.extend(r7.iter().cloned());
    out
}

/// The open phase 2 round in `height` rows with the R7 lines under it. The round's heading, its bound and its
/// candidates come first: R7 keeps its rows only when they fit under all of those, and the round's checks give
/// up rows before it. Rows cut off the round end with a `+N more` row, so cutting any needs a row beyond the
/// candidates.
pub(super) fn round(
    scene: &Scene<'_>,
    width: usize,
    height: usize,
    r7: &[Line<'static>],
) -> Vec<Line<'static>> {
    let (out, head) = phase2_lines(scene, width);
    if r7.is_empty() {
        return bounded_rows(out, height, scene.colours);
    }
    let room = height.saturating_sub(r7.len() + 1);
    if room < out.len() && room < head + 1 {
        return bounded_rows(out, height, scene.colours);
    }
    let mut rows = bounded_rows(out, room, scene.colours);
    rows.push(Line::default());
    rows.extend(r7.iter().cloned());
    rows
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;
    use ratatui::text::Line;
    use togi_journal::Activity;
    use togi_machine::Regime;
    use togi_tuner::{PhaseNo, PhasePlan, SearchTurn};

    use super::super::super::measure::{Measure, measure};
    use super::super::super::scene::Scene;
    use super::super::super::testing::screen;
    use super::super::fixtures::{
        at, bios, core_view, cycle_part, cycle_plan, cycle_step, hunt_facts, hunt_plan, part,
        phases, scene, texts, trial_view,
    };
    use super::{activity_under, context, round};
    use crate::model::{Confirm, CycleView, HuntView, Model, SessionView};
    use crate::screen::{Colours, View};
    use crate::tests::kit::{off, seq};
    use togi_tuner::GroupState;

    fn measured(scene: &Scene<'_>, width: u16, height: u16) -> Measure {
        measure(
            scene,
            &screen(View::Main, width, height, Colours::None, false),
        )
    }

    #[test]
    fn the_turns_are_the_context_when_no_bios_profile_is_shown() {
        let mut view = core_view(11, 1);
        view.confirm = Some(Confirm {
            offset: off(-7),
            light: 1,
            heavy: 0,
            needed: 5,
        });
        let cores = [view];
        let turns = [SearchTurn {
            core: crate::tests::kit::core(11),
            confirm: true,
            regimes: vec![Regime::R1],
            offset: off(-7),
            workload: togi_machine::workloads(Regime::R1)[0].id(),
            step: 1,
            light: 1,
            heavy: 0,
            needed: 5,
            running: true,
        }];
        let model = Model::new();
        let mut s = model.snapshot();
        s.cores = &cores;
        s.turns = &turns;
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let m = measured(&scene, 160, 45);
        let rows = texts(&context(&scene, &m));
        assert!(rows[0].starts_with("TURNS"), "{rows:?}");
        assert!(
            rows.iter()
                .any(|row| row.contains("confirm: light trial 2 of 5"))
        );
    }

    #[test]
    fn the_compact_hunt_gives_up_its_blank_rows_under_the_bios_profile() {
        let plan = hunt_plan(
            Vec::new(),
            vec![
                part(&[0, 1], &[2], Some(4), Some(GroupState::Running), true),
                part(&[2], &[0, 1], None, None, false),
            ],
        );
        let facts = hunt_facts();
        let phases = phases(None);
        let held = bios(true);
        let cores = [core_view(0, 0), core_view(1, 0), core_view(8, 1)];
        let model = Model::new();
        let mut s = model.snapshot();
        s.cores = &cores;
        s.hunt = Some(HuntView {
            plan: &plan,
            facts: &facts,
        });
        s.phases = Some(&phases);
        s.bios = Some(&held);
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let mut m = measured(&scene, 80, 24);
        m.context.height = 30;
        let all = texts(&context(&scene, &m));
        assert!(all[0].starts_with("BIOS PROFILE"));
        assert_eq!(all[5], "");
        assert!(all[6].starts_with("HUNT 1"), "{all:?}");
        m.context.height = 9;
        let tight = texts(&context(&scene, &m));
        assert!(tight.len() <= 9, "{tight:?}");
        assert!(tight[6..].iter().all(|row| !row.is_empty()), "{tight:?}");
        assert!(
            tight.last().is_some_and(|row| row.starts_with('+')),
            "{tight:?}"
        );
    }

    fn r7(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::from(format!("R7 line {i}"))).collect()
    }

    fn cycle_scene_rows(height: u16, r7: &[Line<'static>]) -> Vec<String> {
        let plan = cycle_plan(
            vec![
                cycle_step(
                    Regime::R1,
                    vec![cycle_part(&[0], None, true, false, true)],
                    true,
                    true,
                ),
                cycle_step(
                    Regime::R3,
                    vec![
                        cycle_part(&[0], None, true, false, true),
                        cycle_part(&[1], None, true, true, false),
                    ],
                    false,
                    true,
                ),
            ],
            1,
            false,
        );
        let cores = [core_view(0, 0), core_view(1, 0)];
        let model = Model::new();
        let mut s = model.snapshot();
        s.cores = &cores;
        s.cycle = Some(CycleView {
            plan: &plan,
            step_hunts: &[],
            resume: None,
        });
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let mut m = measured(&scene, 160, 45);
        m.context.height = height;
        texts(&activity_under(&scene, &m, r7))
    }

    #[test]
    fn the_cycle_gives_the_last_rows_to_r7() {
        let rows = cycle_scene_rows(20, &r7(2));
        let n = rows.len();
        assert_eq!(&rows[n - 3..], ["", "R7 line 0", "R7 line 1"]);
        assert!(rows[0].starts_with("CYCLE"));
        let tight = cycle_scene_rows(7, &r7(2));
        assert_eq!(tight.len(), 7, "{tight:?}");
        assert_eq!(&tight[4..], ["", "R7 line 0", "R7 line 1"]);
        assert!(tight.iter().any(|row| row.contains("core 01")), "{tight:?}");
        let none = cycle_scene_rows(20, &[]);
        assert!(none.iter().all(|row| !row.starts_with("R7")));
    }

    #[test]
    fn a_cut_finished_cycle_never_counts_more_than_its_steps() {
        let steps: Vec<_> = (0..18)
            .map(|_| {
                cycle_step(
                    Regime::R1,
                    vec![cycle_part(&[0], None, true, false, true)],
                    true,
                    true,
                )
            })
            .collect();
        let plan = cycle_plan(steps, 18, false);
        let cores = [core_view(0, 0)];
        let model = Model::new();
        let mut s = model.snapshot();
        s.cores = &cores;
        s.cycle = Some(CycleView {
            plan: &plan,
            step_hunts: &[],
            resume: None,
        });
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let mut m = measured(&scene, 80, 24);
        m.context.height = 5;
        let rows = texts(&activity_under(&scene, &m, &r7(2)));
        assert!(rows[0].starts_with("CYCLE 2"), "{rows:?}");
        assert_eq!(rows[1], "+18 more", "{rows:?}");
    }

    fn round_fixture(r7: &[Line<'static>], height: usize) -> Vec<String> {
        let candidates = (0..4u8)
            .map(|id| togi_tuner::Candidate {
                core: crate::tests::kit::core(id),
                offset: off(-30 - i64::from(id)),
                solo_limit: Some(off(-20)),
                gap: u32::from(id) + 1,
                carried: false,
                moving: false,
            })
            .collect();
        let phases = PhasePlan {
            phase: PhaseNo::Two,
            phase1_end: Some(seq(10)),
            concluded: None,
            confirming: false,
            rounds_left: 2,
            round: Some(1),
            candidates,
        };
        let model = Model::new();
        let mut s = model.snapshot();
        s.phases = Some(&phases);
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        texts(&round(&scene, 100, height, r7))
    }

    #[test]
    fn the_round_keeps_its_candidates_at_every_height() {
        let r7 = r7(6);
        for height in 8..60 {
            let rows = round_fixture(&r7, height);
            assert!(
                rows.iter().any(|row| row.contains("gap ")),
                "height {height}: the round lost its candidate rows:\n{}",
                rows.join("\n")
            );
            assert!(rows.len() <= height, "height {height}: {} rows", rows.len());
        }
    }

    #[test]
    fn r7_keeps_its_rows_only_when_they_fit_under_the_candidates() {
        let r7 = r7(3);
        let roomy = round_fixture(&r7, 30);
        assert_eq!(
            &roomy[roomy.len() - 4..],
            ["", "R7 line 0", "R7 line 1", "R7 line 2"]
        );
        assert!(roomy.iter().any(|row| row == "CHECKS"));
        let cramped = round_fixture(&r7, 8);
        assert!(
            cramped.iter().all(|row| !row.starts_with("R7")),
            "{cramped:?}"
        );
        assert!(
            cramped.last().is_some_and(|row| row.starts_with('+')),
            "{cramped:?}"
        );
        let nearly = round_fixture(&r7, 11);
        assert!(
            nearly.iter().all(|row| !row.starts_with("R7")),
            "{nearly:?}"
        );
        let squeezed = round_fixture(&r7, 12);
        assert!(
            squeezed.iter().any(|row| row.starts_with("R7 line 0")),
            "{squeezed:?}"
        );
        assert!(
            squeezed.iter().any(|row| row.starts_with('+')),
            "{squeezed:?}"
        );
    }

    #[test]
    fn a_deepening_session_with_an_open_round_shows_the_round() {
        let candidates = vec![togi_tuner::Candidate {
            core: crate::tests::kit::core(0),
            offset: off(-30),
            solo_limit: Some(off(-20)),
            gap: 1,
            carried: false,
            moving: true,
        }];
        let phases = PhasePlan {
            phase: PhaseNo::Two,
            phase1_end: Some(seq(10)),
            concluded: None,
            confirming: false,
            rounds_left: 1,
            round: Some(2),
            candidates,
        };
        let session = SessionView {
            start: at(1000),
            activity: Some(Activity::Deepening),
        };
        let model = Model::new();
        let mut s = model.snapshot();
        let trial = trial_view(1, 5);
        s.session = Some(&session);
        s.trial = Some(&trial);
        s.phases = Some(&phases);
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        let m = measured(&scene, 160, 45);
        let rows = texts(&activity_under(&scene, &m, &[]));
        assert!(rows[0].starts_with("PHASE 2 · ROUND 2"), "{rows:?}");
    }
}
