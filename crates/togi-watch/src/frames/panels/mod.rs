//! The panels behind the detail pages: the cycle and its parts, a hunt with its groups and probes, the search
//! turns, a phase 2 round, the combinations, the BIOS profile and the history; and the journal view.

mod cycle;
#[cfg(test)]
mod fixtures;
mod history;
mod hunt;
mod log;
mod pages;
mod search;
mod table;

use jiff::tz::TimeZone;
use ratatui::text::Line;

use super::fit::Clause;
use super::layout::SizeClass;
use super::scene::Scene;
use crate::screen::{Colours, Log, View};

/// Main's folded What happened: at most `height` rows of `width` cells, newest meaningful first.
pub(crate) fn main_history(scene: &Scene<'_>, width: usize, height: usize) -> Vec<Line<'static>> {
    history::main_history(scene, width, height)
}

/// The History page's body in `width` cells, unscrolled: expanded entries with local date separators.
pub(crate) fn history_page(scene: &Scene<'_>, width: usize) -> Vec<Line<'static>> {
    history::history_page(scene, width)
}

/// Where the stage line says a hunt stands: `group 2 of 3`, `part 1 of 2`; empty when it says nothing.
pub(crate) fn hunt_stage(scene: &Scene<'_>, short: bool) -> String {
    scene
        .s
        .hunt
        .as_ref()
        .map_or_else(String::new, |hunt| hunt::stage(hunt, short))
}

/// The journal view's body in `width` cells and `height` rows scrolled `scroll` lines down, with its
/// scrollbar; and how far it can scroll. A scroll at or past that extent shows the end.
pub(crate) fn log_body(
    log: &Log<'_>,
    width: usize,
    height: usize,
    scroll: usize,
    zone: &TimeZone,
    colours: Colours,
) -> (Vec<Line<'static>>, usize) {
    log::body(log, width, height, scroll, zone, colours)
}

/// The footer clause that counts the events arrived since the journal view held its list; none when none
/// arrived.
pub(crate) fn log_arrivals(log: &Log<'_>, colours: Colours) -> Vec<Clause> {
    log::arrivals(log, colours)
}

/// The body of detail page `view` (Cycle, R7 evidence, Hunts / probes or BIOS profile) in `width` cells,
/// unscrolled, with `class`'s table columns; an explicit empty-state sentence when the page has nothing to show yet.
/// The other views have no body here.
pub(crate) fn page_lines(
    scene: &Scene<'_>,
    view: View,
    width: usize,
    class: SizeClass,
) -> Vec<Line<'static>> {
    pages::page_lines(scene, view, width, class)
}
