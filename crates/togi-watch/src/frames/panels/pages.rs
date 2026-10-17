//! The detail pages built from the panels: Cycle, R7 evidence, Hunts / probes and BIOS profile.

use ratatui::text::Line;
use togi_journal::Activity;

use super::super::colours::Paint;
use super::super::dash::r7_lines;
use super::super::fit::wrap_styled;
use super::super::layout::SizeClass;
use super::super::measure::Tables;
use super::super::scene::Scene;
use super::cycle::{combination_lines, cycle_lines};
use super::hunt::hunt_lines;
use super::search::{bios_page, phase2_lines, turn_lines};
use crate::screen::View;

const NO_SESSION: &str =
    "No session has started yet: the current phase's work list appears here once tuning starts.";
const NO_WORK: &str = "No work list yet: search turns, the cycle checklist or a phase 2 round appear here once the tuner has one.";
const NO_HUNT: &str = "No hunt is running and none is recorded in this session. A hunt starts when a multi-core failure needs its cause located; its cause, groups and probes appear here.";

fn sentence(scene: &Scene<'_>, text: &str, width: usize) -> Vec<Line<'static>> {
    wrap_styled(text, width, scene.style(Paint::Grey))
}

/// The body of detail page `view` in `width` cells, unscrolled; other pages have no body here.
pub(super) fn page_lines(
    scene: &Scene<'_>,
    view: View,
    width: usize,
    class: SizeClass,
) -> Vec<Line<'static>> {
    let class = class.max(fitting_class(width));
    match view {
        View::Cycle => cycle_page(scene, width, class),
        View::R7 => r7_lines(scene, width, class),
        View::Hunts => hunts_page(scene, width, class),
        View::Bios => bios_page(scene, width),
        View::Main | View::Cores | View::History | View::Log | View::Help => Vec::new(),
    }
}

/// The widest table layout the page's own width holds whole: a page is not limited to the class of the screen,
/// whose size also counts rows.
fn fitting_class(width: usize) -> SizeClass {
    if width >= 120 {
        SizeClass::Wide
    } else if width >= 80 {
        SizeClass::Medium
    } else {
        SizeClass::Compact
    }
}

/// The current phase's work list: the search turns, the open phase 2 round, or the cycle checklist with the
/// combinations under it.
fn cycle_page(scene: &Scene<'_>, width: usize, class: SizeClass) -> Vec<Line<'static>> {
    if scene.s.session.is_none() {
        return sentence(scene, NO_SESSION, width);
    }
    let t = Tables::new(class, width);
    let deepening = scene
        .s
        .session
        .is_some_and(|session| session.activity == Some(Activity::Deepening));
    let open = scene
        .s
        .phases
        .is_some_and(|phases| phases.round.is_some_and(|round| round > 0));
    let mut out = if !scene.s.turns.is_empty() {
        turn_lines(scene, &t, width, class)
    } else if deepening && open {
        phase2_lines(scene, width)
    } else if let Some(cycle) = &scene.s.cycle {
        cycle_lines(scene, cycle, &t, width, class)
    } else {
        Vec::new()
    };
    if !scene.s.combos.is_empty() {
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.extend(combination_lines(scene, &t, width, class));
    }
    if out.is_empty() {
        return sentence(scene, NO_WORK, width);
    }
    out
}

/// The running hunt with its probes, else an explicit statement that none is running.
fn hunts_page(scene: &Scene<'_>, width: usize, class: SizeClass) -> Vec<Line<'static>> {
    let t = Tables::new(class, width);
    match &scene.s.hunt {
        Some(hunt) => hunt_lines(scene, hunt, &t, width, class),
        None => sentence(scene, NO_HUNT, width),
    }
}
