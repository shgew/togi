//! The panels under the core tables: the BIOS profile, the cycle and its parts, a hunt with its groups and
//! probes, the search turns, a phase 2 round, the combinations and the history; and the journal view.

mod context;
mod cycle;
#[cfg(test)]
mod fixtures;
mod history;
mod hunt;
mod log;
mod search;
mod table;

use jiff::tz::TimeZone;
use ratatui::text::Line;

use super::fit::Clause;
use super::measure::Measure;
use super::scene::Scene;
use crate::screen::{Colours, Log};

/// The context panel's rows: the BIOS profile with the panel under it (a hunt, the search turns, a round, or the
/// cycle, the combinations and R7), at most `measure.context.height` of them.
pub(crate) fn context_lines(scene: &Scene<'_>, measure: &Measure) -> Vec<Line<'static>> {
    context::context(scene, measure)
}

/// What happened, newest first, in `height` rows of `width` cells. Its last row counts every entry it leaves
/// out: those that do not fit and those older than the entries the model keeps.
pub(crate) fn history_panel(scene: &Scene<'_>, width: usize, height: usize) -> Vec<Line<'static>> {
    history::history(scene, width, height)
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
