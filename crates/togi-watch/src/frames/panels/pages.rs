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
const HUNT_EXPLAINED: &str = "A hunt starts when a multi-core failure needs its cause located; its cause, groups and probes appear here.";

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

/// The running hunt with its probes, else an explicit statement that none is running: that none was recorded
/// in the kept history, or which hunt was the latest recorded and where its story is.
fn hunts_page(scene: &Scene<'_>, width: usize, class: SizeClass) -> Vec<Line<'static>> {
    let t = Tables::new(class, width);
    match &scene.s.hunt {
        Some(hunt) => hunt_lines(scene, hunt, &t, width, class),
        None => sentence(scene, &no_hunt(scene), width),
    }
}

fn no_hunt(scene: &Scene<'_>) -> String {
    let history = View::History.number();
    let latest = scene
        .s
        .history
        .iter()
        .filter_map(|entry| entry.hunt)
        .chain(scene.s.shapes.keys().copied())
        .max();
    match latest {
        Some(hunt) => format!(
            "No hunt is running. The latest recorded hunt is hunt {hunt}; page {history} lists what it did."
        ),
        None if scene.s.history_dropped > 0 => format!(
            "No hunt is running and none is recorded in the kept history; page {history} lists what is kept. {HUNT_EXPLAINED}"
        ),
        None => format!(
            "No hunt is running and none has been recorded in this session. {HUNT_EXPLAINED}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use jiff::tz::TimeZone;

    use super::super::super::fit::plain;
    use super::super::super::layout::SizeClass;
    use super::super::super::scene::Scene;
    use super::page_lines;
    use crate::history::Entry;
    use crate::model::Model;
    use crate::screen::{Colours, View};
    use crate::words::{Tag, Tone};

    fn hunts_page(entries: &[Entry], dropped: usize) -> String {
        let model = Model::new();
        let mut s = model.snapshot();
        s.history = entries;
        s.history_dropped = dropped;
        let zone = TimeZone::UTC;
        let scene = Scene::new(s, Timestamp::UNIX_EPOCH, &zone, Colours::None);
        page_lines(&scene, View::Hunts, 200, SizeClass::Wide)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn hunt_entry(hunt: u32) -> Entry {
        let mut entry = Entry::line(
            Timestamp::UNIX_EPOCH,
            Tag::Hunt,
            format!("#{hunt} started"),
            Tone::Warn,
        );
        entry.hunt = Some(hunt);
        entry
    }

    #[test]
    fn the_hunts_page_without_a_hunt_says_none_was_recorded() {
        assert_eq!(
            hunts_page(&[], 0),
            "No hunt is running and none has been recorded in this session. A hunt starts when a multi-core failure needs its cause located; its cause, groups and probes appear here."
        );
        assert_eq!(
            hunts_page(&[], 3),
            "No hunt is running and none is recorded in the kept history; page 7 lists what is kept. A hunt starts when a multi-core failure needs its cause located; its cause, groups and probes appear here."
        );
    }

    #[test]
    fn the_hunts_page_after_a_finished_hunt_names_the_latest_and_points_to_history() {
        let story = [hunt_entry(2), hunt_entry(1)];
        assert_eq!(
            hunts_page(&story, 0),
            "No hunt is running. The latest recorded hunt is hunt 2; page 7 lists what it did."
        );
    }
}
