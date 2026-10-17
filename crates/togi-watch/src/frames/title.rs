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
    use test_case::test_case;

    use super::page_title;
    use crate::frames::fit::plain;
    use crate::screen::{Colours, View};

    fn title(view: View, width: usize) -> String {
        plain(&page_title(view, width, Colours::None))
    }

    #[test_case(View::Main, "MAIN · 1/9 · < Help | Cores >"; "main")]
    #[test_case(View::Cores, "CORES · 2/9 · < Main | Cycle >"; "cores")]
    #[test_case(View::Cycle, "CYCLE · 3/9 · < Cores | R7 evidence >"; "cycle")]
    #[test_case(View::R7, "R7 EVIDENCE · 4/9 · < Cycle | Hunts / probes >"; "r7")]
    #[test_case(View::Hunts, "HUNTS / PROBES · 5/9 · < R7 evidence | BIOS profile >"; "hunts")]
    #[test_case(View::Bios, "BIOS PROFILE · 6/9 · < Hunts / probes | History >"; "bios")]
    #[test_case(View::History, "HISTORY · 7/9 · < BIOS profile | Journal >"; "history")]
    #[test_case(View::Log, "JOURNAL · 8/9 · < History | Help >"; "log")]
    #[test_case(View::Help, "HELP · 9/9 · < Journal | Main >"; "help")]
    fn every_page_names_itself_its_place_and_its_neighbours(view: View, want: &str) {
        assert_eq!(title(view, 80), want);
    }

    #[test_case(29, "CORES · 2/9 · <Main|Cycle>"; "the compact neighbours")]
    #[test_case(25, "CORES · 2/9"; "no neighbours")]
    #[test_case(11, "CORES · 2/9"; "the place only")]
    #[test_case(10, "CORES"; "the name only")]
    #[test_case(5, "CORES"; "the name in five cells")]
    fn a_narrow_row_drops_whole_clauses_and_keeps_the_page_name(width: usize, want: &str) {
        assert_eq!(title(View::Cores, width), want);
    }
}
