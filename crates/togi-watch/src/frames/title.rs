//! The title row of a page: its name, its place in the cycle and the pages on either side.

use ratatui::text::Line;

use super::colours::Paint;
use super::fit::{Clause, Form, fit_clauses};
use crate::screen::{Colours, View};

const SEP: &str = " · ";

/// The title of `view` in `width` cells: `CORES · 2/9 · < Main | Cycle >`. A narrow row gives the neighbours up
/// first, whole, then the place in the cycle; the page's name stays.
pub(crate) fn page_title(view: View, width: usize, colours: Colours) -> Line<'static> {
    let paint = |tone: Paint, text: String| Line::from(tone.span(colours, text));
    let (before, after) = (view.prev().name(), view.next().name());
    fit_clauses(
        width,
        &[
            Clause::whole("", paint(Paint::Text, view.name().to_uppercase())),
            Clause::new(
                paint(Paint::Grey, SEP.to_owned()),
                vec![
                    Form::new(
                        paint(
                            Paint::Grey,
                            format!("{}/{}", view.number(), View::ALL.len()),
                        ),
                        1,
                    ),
                    Form::empty(),
                ],
            ),
            Clause::new(
                paint(Paint::Grey, SEP.to_owned()),
                vec![
                    Form::new(paint(Paint::Grey, format!("< {before} | {after} >")), 3),
                    Form::new(paint(Paint::Grey, format!("<{before}|{after}>")), 2),
                    Form::empty(),
                ],
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::page_title;
    use crate::frames::fit::plain;
    use crate::screen::{Colours, View};

    fn title(view: View, width: usize) -> String {
        plain(&page_title(view, width, Colours::None))
    }

    #[test]
    fn every_page_names_itself_its_place_and_its_neighbours() {
        for (view, want) in [
            (View::Main, "MAIN · 1/9 · < Help | Cores >"),
            (View::Cores, "CORES · 2/9 · < Main | Cycle >"),
            (View::Cycle, "CYCLE · 3/9 · < Cores | R7 evidence >"),
            (View::R7, "R7 EVIDENCE · 4/9 · < Cycle | Hunts / probes >"),
            (
                View::Hunts,
                "HUNTS / PROBES · 5/9 · < R7 evidence | BIOS profile >",
            ),
            (
                View::Bios,
                "BIOS PROFILE · 6/9 · < Hunts / probes | History >",
            ),
            (View::History, "HISTORY · 7/9 · < BIOS profile | Journal >"),
            (View::Log, "JOURNAL · 8/9 · < History | Help >"),
            (View::Help, "HELP · 9/9 · < Journal | Main >"),
        ] {
            assert_eq!(title(view, 80), want);
        }
    }

    #[test]
    fn a_narrow_row_drops_whole_clauses_and_keeps_the_page_name() {
        assert_eq!(title(View::Cores, 29), "CORES · 2/9 · <Main|Cycle>");
        assert_eq!(title(View::Cores, 25), "CORES · 2/9");
        assert_eq!(title(View::Cores, 11), "CORES · 2/9");
        assert_eq!(title(View::Cores, 10), "CORES");
        assert_eq!(title(View::Cores, 5), "CORES");
    }
}
