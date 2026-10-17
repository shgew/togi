//! The help page: static explanations of the pages, keys and gauges, in three columns when wide and one scrolling
//! column otherwise.
//!
//! Labels keep whole words at every width. The page shows a scrollbar whenever its contents need scrolling, and
//! scrolling to the end reaches the last text row, not trailing blank rows.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::colours::Paint;
use super::fit::{cut_words, display_width, line_width, truncate_styled, wrap_styled};
use super::scroll::scroll_body_with_bar;
use crate::screen::{Colours, Quit};

struct Section<'a> {
    title: &'static str,
    items: &'a [(&'static str, &'static str)],
}

const NAVIGATION: Section = Section {
    title: "NAVIGATION",
    items: &[
        (
            "← →",
            "Move through the nine pages in a fixed cycle that wraps, from Main to Help and back to Main. The footer shows the page's place, such as 2/9 Cores.",
        ),
        ("↑ ↓", "Scroll a page one line. j and k do the same."),
        (
            "PgUp PgDn",
            "Scroll a page by its height less one line. b, f and space do the same.",
        ),
        (
            "Home End",
            "Go to the top or the end of a page. g and G do the same.",
        ),
        ("Esc, m", "Go back to Main."),
        (
            "?, h",
            "Open this help. The same key on the help returns to Main.",
        ),
        (
            "l",
            "Open the journal, which opens at its newest event and follows new ones. The same key on the journal returns to Main.",
        ),
        (
            "scroll",
            "A page opens at its top. Only a key changes the page or its scroll, so a refresh never moves it. Main scrolls only on a screen smaller than 160x45, where it shows numbers instead of gauges.",
        ),
    ],
};

/// The navigation row for the quit keys: the keys that exit under [`Quit::Keys`], and the plain fact that there
/// are none under [`Quit::Ignored`].
const EXIT: (&str, &str) = (
    "q, Ctrl-C",
    "Exit the dashboard; tuning itself keeps running.",
);
const NO_EXIT: (&str, &str) = (
    "exit",
    "This dashboard has no exit key: only stopping its service ends it. Tuning itself keeps running.",
);

const PAGES: Section = Section {
    title: "THE PAGES",
    items: &[
        (
            "1 Main",
            "Which phase and how far through it, what runs now, every core's gauge and a short What happened.",
        ),
        (
            "2 Cores",
            "Each core's exact tune, now, solo, gap and failure, its role and where its failure came from.",
        ),
        (
            "3 Cycle",
            "The current phase's work list: search turns, the cycle's steps or the phase 2 round.",
        ),
        (
            "4 R7 evidence",
            "R7 request order, top requesters and which cores passed as top requester.",
        ),
        (
            "5 Hunts / probes",
            "The cause of a failure and how sure the attribution is, hunt groups, splits and probe evidence.",
        ),
        (
            "6 BIOS profile",
            "The profile to enter in BIOS, one row per CCD in slot order. A * marks an offset not yet confirmed.",
        ),
        (
            "7 History",
            "What happened, expanded, with the local date of each day.",
        ),
        (
            "8 Journal",
            "The newest 400 journal events. Scrolled back, it holds its list still and counts new events until End.",
        ),
        ("9 Help", "This page."),
    ],
};

const GAUGES: Section = Section {
    title: "THE GAUGES",
    items: &[
        (
            "tune",
            "The tuning profile's offset for the core: a white full-height █ bar, one cell per count, -1 at the left to -50 at the right. It is drawn from the profile in every phase, never from what is applied. A core at 0 has an empty bar.",
        ),
        (
            "now",
            "The offset observed applied to the hardware now. After a stop, now shows the offset the stop restored, such as 0 for a zero baseline, while tune keeps the profile. now ? means no application was observed; it is not a zero.",
        ),
        (
            "red point",
            "One full-height red █ in a lane of its own under the bar, at the depth of the recorded failure constraint. No recorded failure reads f none; a failure at 0 reads f0 and draws no cell.",
        ),
        (
            "solo",
            "The deepest offset the core confirmed alone under load. s? means none is known, and then g? too.",
        ),
        (
            "gap",
            "Tune minus solo, in counts shallower than solo: tune -9 and solo -45 is gap 36. It is a number, not a verdict. A negative gap means tune is deeper than solo.",
        ),
        (
            "fail",
            "The recorded failure constraint as an offset: f-50 is -50, f none is no failure recorded.",
        ),
        (
            "role",
            "What the core does now: loaded, judged, idle at 0 or reset queued.",
        ),
        (
            "!",
            "A current failure, shown until work moves to another step or operation.",
        ),
        (
            "R",
            "A reset is queued for the core. It does not reopen a stopped session.",
        ),
        ("cyan", "running now"),
        ("amber", "a hunt or a repair, and what it paused"),
        ("red", "failures, past and present"),
        ("green", "passed, done"),
        ("grey", "idle, parked, still to come"),
    ],
};

const SOURCES: Section = Section {
    title: "FAILURE SOURCES",
    items: &[
        (
            "alone",
            "The core failed by itself, loaded alone. Its red point is that failure.",
        ),
        (
            "named",
            "A failure named this core directly, or a hunt traced a group's failure to it.",
        ),
        (
            "R7-charged",
            "An all-core R7 failure the decision charged to this core. The attribution may be uncertain.",
        ),
        (
            "constraint",
            "A red point is a recorded failure constraint, not always a core that failed alone. The Cores page names the source and the journal entry.",
        ),
    ],
};

const TOP: Section = Section {
    title: "THE TOP LINE",
    items: &[
        (
            "failures",
            "failure decisions, including trials skipped because they already failed",
        ),
        ("crashes", "boots that ended without a clean shutdown"),
        (
            "last observed",
            "the last failure seen in this session that changed what togi does; skips do not count",
        ),
    ],
};

const TUNING: Section = Section {
    title: "WHAT TOGI DOES",
    items: &[
        (
            "SOLO LIMITS",
            "Finding each core's solo limit alone. The count is the solo limits found, never time.",
        ),
        (
            "PHASE 1",
            "Checking toward the first confirmed profile, counted in the steps of its cycle.",
        ),
        (
            "PHASE 2",
            "Deepening in rounds, counted in credited checks, with its confirmation cycle: PHASE 2 · confirmation cycle.",
        ),
        (
            "CLEAN CYCLES",
            "The count of clean cycles for the current profile. It has no percentage and no ETA.",
        ),
        (
            "Cycle",
            "Not a phase: the list of steps a phase checks through, on the Cycle page.",
        ),
        (
            "HUNT",
            "An amber detour inside a phase, not a phase of its own: the parent phase stays current while parked trials find the core or combination behind a failure.",
        ),
        (
            "STOPPED",
            "STOPPED or RECOVERY replaces every running marker on the stage line. A stop keeps the stages already completed.",
        ),
        (
            "forecasts",
            "What a result would lead to is always conditional, neutral in colour and no promise. A hunt can start at any time, so there is no ETA.",
        ),
        (
            "R6",
            "During idle trials every page holds still, clock included, so it cannot wake the cores.",
        ),
        (
            "RULES",
            "How togi decides what to do: https://github.com/shgew/togi/blob/main/docs/spec/tuner.md",
        ),
    ],
};

const WORDS: Section = Section {
    title: "WORDS",
    items: &[
        (
            "SEARCH",
            "finding its solo limit with light and heavy-vector trials alone",
        ),
        (
            "CONFIRM",
            "checking its candidate solo limit with repeated light and heavy-vector trials alone",
        ),
        ("FOUND", "solo limit checked; waiting at 0"),
        ("WAITING", "at 0, waiting for its first search turn"),
        (
            "MEMBER",
            "kept with its hunt group at its failing offset; another member may be probed",
        ),
        (
            "offset",
            "Curve Optimizer counts, 0 to -50; deeper is more negative",
        ),
        ("trial", "one launch of one workload on its target"),
        (
            "solo limit",
            "the deepest offset a core confirmed under load alone",
        ),
        (
            "failure constraint",
            "the shallowest offset recorded as ruled out for a core",
        ),
        ("suspect", "a core a hunt keeps at its failing offset"),
        (
            "parked",
            "a core a hunt holds no deeper than its failing offset, outside the selected group",
        ),
        ("partial part", "an R7 part with the top requesters idle"),
        (
            "top requester",
            "an R7 workload's highest-request group on each CCD, ties included; marked top on the CCD tables",
        ),
        (
            "self-sufficient",
            "observed passing as top requester for this R7 workload",
        ),
        ("cycle", "one pass through the checking schedule"),
    ],
};

const LOAD: Section = Section {
    title: "KINDS OF LOAD",
    items: &[
        ("R1 light", "one light thread: compiling, browsing"),
        (
            "R2 heavy vector",
            "one AVX2/AVX-512 thread: encoding, rendering",
        ),
        ("R3 load steps", "load switching on and off: current steps"),
        ("R4 partial load", "light load at 25-75% duty: bursty work"),
        ("R5 both threads", "both threads of one core: SMT pressure"),
        (
            "R6 idle + bursts",
            "idle with short wake-ups: idle and boost",
        ),
        (
            "R7 all-core",
            "a CCD, part of one, or every core: power and heat",
        ),
    ],
};

/// The body width from which the help takes three columns.
const THREE_COLUMNS: usize = 195;

/// A rule across `width` cells: the whole words of `title` that fit in white, then a grey dash track.
#[must_use]
pub fn title_rule(title: &str, width: usize, colours: Colours) -> Line<'static> {
    let title = cut_words(title, width);
    let used = display_width(&title);
    let mut spans = Vec::new();
    if !title.is_empty() {
        spans.push(Paint::White.span(colours, title));
    }
    if used < width {
        spans.push(Span::raw(" "));
        spans.push(Paint::Grey.span(colours, "─".repeat(width - used - 1)));
    }
    Line::from(spans)
}

fn blank() -> Line<'static> {
    Line::default()
}

fn section_lines(
    section: &Section<'_>,
    width: usize,
    spaced: bool,
    colours: Colours,
) -> Vec<Line<'static>> {
    let mut out = vec![title_rule(section.title, width, colours), blank()];
    let widest = section
        .items
        .iter()
        .map(|&(label, _)| display_width(label))
        .max()
        .unwrap_or(0);
    let label_width = (widest + 2).min((width / 3).max(1));
    for (i, &(label, text)) in section.items.iter().enumerate() {
        #[cfg(test)]
        crate::corpus::text("help.item", section.title, text);
        let wrapped = wrap_styled(
            text,
            width.saturating_sub(label_width).max(1),
            Paint::Text.style(colours),
        );
        for (row, line) in wrapped.into_iter().enumerate() {
            let mut spans = if row == 0 {
                let name = cut_words(label, label_width.saturating_sub(1));
                let pad = label_width.saturating_sub(display_width(&name));
                vec![Paint::White.span(colours, name), Span::raw(" ".repeat(pad))]
            } else {
                vec![Span::raw(" ".repeat(label_width))]
            };
            spans.extend(line.spans);
            out.push(truncate_styled(&Line::from(spans), width));
        }
        if spaced && i + 1 < section.items.len() {
            out.push(blank());
        }
    }
    out
}

/// The help body in `width` cells and `height` rows, scrolled `scroll` lines down, with its scrollbar in the
/// first two cells of every row; and how far it can scroll. A scroll at or past that extent shows the end. Under
/// [`Quit::Ignored`] the navigation says there is no exit key instead of naming one.
#[must_use]
pub fn help_body(
    width: usize,
    height: usize,
    scroll: usize,
    colours: Colours,
    quit: Quit,
) -> (Vec<Line<'static>>, usize) {
    let navigation_items: Vec<_> = NAVIGATION
        .items
        .iter()
        .copied()
        .chain([if quit == Quit::Keys { EXIT } else { NO_EXIT }])
        .collect();
    let navigation = Section {
        title: NAVIGATION.title,
        items: &navigation_items,
    };
    let sections = [
        &navigation,
        &PAGES,
        &GAUGES,
        &SOURCES,
        &TUNING,
        &TOP,
        &WORDS,
        &LOAD,
    ];
    let body_width = width.saturating_sub(2).max(1);
    let lines = if width >= THREE_COLUMNS {
        let usable = body_width - 8;
        let left = usable * 81 / 227;
        let middle = usable * 75 / 227;
        let right = usable - left - middle;
        let widths = [left, middle, right];
        let mut columns: [Vec<Line<'static>>; 3] = Default::default();
        for section in sections {
            let at = (0..columns.len())
                .min_by_key(|&column| columns[column].len())
                .unwrap_or(0);
            if !columns[at].is_empty() {
                columns[at].extend([blank(), blank()]);
            }
            columns[at].extend(section_lines(
                section,
                widths[at],
                section.title == TUNING.title,
                colours,
            ));
        }
        let count = columns.iter().map(Vec::len).max().unwrap_or(0);
        (0..count)
            .map(|row| {
                let mut spans = Vec::new();
                for (col, column) in columns.iter().enumerate() {
                    let cell = column.get(row);
                    let used = cell.map_or(0, line_width);
                    if let Some(cell) = cell {
                        spans.extend(cell.spans.iter().cloned());
                    }
                    spans.push(Span::styled(
                        " ".repeat(widths[col].saturating_sub(used)),
                        Style::new(),
                    ));
                    if col < 2 {
                        spans.push(Span::raw("    "));
                    }
                }
                Line::from(spans)
            })
            .collect()
    } else {
        let mut lines = Vec::new();
        for section in sections {
            if !lines.is_empty() {
                lines.extend([blank(), blank()]);
            }
            lines.extend(section_lines(
                section,
                body_width,
                section.title == TUNING.title,
                colours,
            ));
        }
        lines
    };
    scroll_body_with_bar(&lines, width, height, scroll, colours)
}

#[cfg(test)]
mod tests {
    use super::{LOAD, Section, TUNING, help_body, section_lines, title_rule};
    use crate::frames::body_rows;
    use crate::frames::fit::{line_width, plain};
    use crate::frames::layout::Layout;
    use crate::frames::testing::{
        assert_frame_bounds, assert_glyphs_drawable, render_missing, rows_of, screen,
    };
    use crate::screen::{Colours, Drawn, Hold, Quit, View};
    use test_case::test_case;

    const C: Colours = Colours::Full;

    const TITLES: [&str; 8] = [
        "NAVIGATION",
        "THE PAGES",
        "THE GAUGES",
        "FAILURE SOURCES",
        "WHAT TOGI DOES",
        "THE TOP LINE",
        "WORDS",
        "KINDS OF LOAD",
    ];

    fn plain_rows(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines.iter().map(plain).collect()
    }

    /// The help in one column as words, with the scrollbar's two cells dropped.
    fn words(quit: Quit) -> String {
        plain_rows(&help_body(115, 1000, 0, C, quit).0)
            .iter()
            .map(|row| row.chars().skip(2).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn chars_from(text: &str, from: usize) -> String {
        text.chars().skip(from).collect()
    }

    fn first_char(text: &str) -> char {
        text.chars().next().unwrap_or(' ')
    }

    #[test]
    fn wide_help_uses_three_columns_and_every_section_is_in_it() {
        let (body, scroll) = help_body(235, 1000, 0, C, Quit::Keys);
        assert_eq!(scroll, 0, "full wide help needs no scrolling");
        let rows = plain_rows(&body);
        let columns = ["NAVIGATION", "THE PAGES", "THE GAUGES"]
            .iter()
            .filter(|title| rows[0].contains(**title))
            .count();
        assert_eq!(columns, 3, "wide help lost a column: {:?}", rows[0]);
        let text = rows.join("\n");
        for title in TITLES {
            assert!(text.contains(title), "wide help lost {title:?}");
        }
        for (row, line) in body.iter().enumerate() {
            assert!(line_width(line) <= 235, "row {row} exceeds help rectangle");
        }
        assert!(
            !rows[rows.len() - 1].trim().is_empty(),
            "wide help ends on a blank content row"
        );
        let single = help_body(115, 1000, 0, C, Quit::Keys).0.len();
        assert!(
            body.len() < single,
            "three columns are shorter than one: {} against {single}",
            body.len()
        );
    }

    #[test_case(195; "195 columns")]
    #[test_case(235; "235 columns")]
    fn short_wide_help_scrolls_with_a_scrollbar_to_its_last_row(width: usize) {
        let (top, scroll) = help_body(width, 19, 0, C, Quit::Keys);
        let (end, end_scroll) = help_body(width, 19, usize::MAX, C, Quit::Keys);
        let (clamped, _) = help_body(width, 19, scroll + 100, C, Quit::Keys);
        assert!(
            scroll != 0 && end_scroll == scroll,
            "short wide help must scroll: top={scroll} end={end_scroll}"
        );
        assert_eq!(plain_rows(&end), plain_rows(&clamped), "scroll beyond end");
        assert!(plain(&top[0]).contains("NAVIGATION"));
        let thumb = (19 * 19 / (scroll + 19)).max(1);
        for (name, page, bar) in [
            (
                "top",
                &top,
                format!("{}{}", "█".repeat(thumb), "│".repeat(19 - thumb)),
            ),
            (
                "end",
                &end,
                format!("{}{}", "│".repeat(19 - thumb), "█".repeat(thumb)),
            ),
        ] {
            assert_eq!(page.len(), 19, "{name} page rows");
            let mut got = String::new();
            for (row, line) in page.iter().enumerate() {
                assert!(
                    line_width(line) <= width,
                    "{name} row {row} exceeds the help rectangle"
                );
                let text = plain(line);
                assert_eq!(
                    text.chars().nth(1),
                    Some(' '),
                    "{name} row {row} loses the scrollbar's two columns: {text:?}"
                );
                got.push(first_char(&text));
            }
            assert_eq!(got, bar, "{name} scrollbar");
        }
        let last = plain(&end[end.len() - 1]);
        assert!(
            !chars_from(&last, 2).trim().is_empty(),
            "short wide help ends on a blank content row"
        );
    }

    #[test]
    fn help_explains_the_pages_and_every_key() {
        let help = words(Quit::Keys);
        for said in [
            "← → Move through the nine pages in a fixed cycle that wraps, from Main to Help and back to Main.",
            "↑ ↓ Scroll a page one line. j and k do the same.",
            "PgUp PgDn Scroll a page by its height less one line. b, f and space do the same.",
            "Home End Go to the top or the end of a page. g and G do the same.",
            "Esc, m Go back to Main.",
            "?, h Open this help. The same key on the help returns to Main.",
            "l Open the journal, which opens at its newest event and follows new ones.",
            "A page opens at its top.",
            "a refresh never moves it.",
            "1 Main",
            "2 Cores",
            "3 Cycle",
            "4 R7 evidence",
            "5 Hunts / probes",
            "6 BIOS profile",
            "7 History",
            "8 Journal",
            "9 Help This page.",
            "A * marks an offset not yet confirmed.",
        ] {
            assert!(help.contains(said), "help lost {said:?}: {help}");
        }
    }

    #[test]
    fn help_explains_the_profile_the_applied_offset_and_the_gauge_grammar() {
        let help = words(Quit::Keys);
        for said in [
            "tune The tuning profile's offset for the core: a white full-height █ bar, one cell per count, -1 at the left to -50 at the right.",
            "never from what is applied.",
            "now The offset observed applied to the hardware now.",
            "After a stop, now shows the offset the stop restored, such as 0 for a zero baseline, while tune keeps the profile.",
            "now ? means no application was observed; it is not a zero.",
            "One full-height red █ in a lane of its own under the bar, at the depth of the recorded failure constraint.",
            "No recorded failure reads f none; a failure at 0 reads f0 and draws no cell.",
            "gap Tune minus solo, in counts shallower than solo: tune -9 and solo -45 is gap 36. It is a number, not a verdict.",
            "A negative gap means tune is deeper than solo.",
            "s? means none is known, and then g? too.",
            "! A current failure",
            "R A reset is queued for the core.",
        ] {
            assert!(help.contains(said), "help lost {said:?}: {help}");
        }
    }

    #[test]
    fn help_names_the_failure_sources_the_phases_and_the_conditional_forecasts() {
        let help = words(Quit::Keys);
        for said in [
            "alone The core failed by itself, loaded alone.",
            "named A failure named this core directly, or a hunt traced a group's failure to it.",
            "R7-charged An all-core R7 failure the decision charged to this core.",
            "not always a core that failed alone.",
            "SOLO LIMITS Finding each core's solo limit alone.",
            "PHASE 1 Checking toward the first confirmed profile",
            "PHASE 2 Deepening in rounds",
            "PHASE 2 · confirmation cycle.",
            "CLEAN CYCLES The count of clean cycles for the current profile.",
            "Cycle Not a phase",
            "HUNT An amber detour inside a phase, not a phase of its own",
            "STOPPED STOPPED or RECOVERY replaces every running marker on the stage line.",
            "forecasts What a result would lead to is always conditional, neutral in colour and no promise.",
        ] {
            assert!(help.contains(said), "help lost {said:?}: {help}");
        }
    }

    #[test]
    fn help_has_no_zoom_and_describes_no_panel_the_main_page_lost() {
        for quit in [Quit::Keys, Quit::Ignored] {
            let help = words(quit);
            for gone in [
                "zoom",
                "+/-",
                "2 counts",
                "two counts",
                "context panel",
                "outcome",
                "closes this view",
                "record only",
                "record-only",
                "its failure point",
                "READING THE SCREEN",
                "▄",
                "░",
                "▬",
                "▀",
            ] {
                assert!(!help.contains(gone), "help still says {gone:?}: {help}");
            }
        }
    }

    #[test]
    fn help_words_claim_no_more_than_the_journal_shows() {
        let help = words(Quit::Keys);
        for said in [
            "changed what togi does; skips do not count",
            "the shallowest offset recorded as ruled out for a core",
            "a core a hunt holds no deeper than its failing offset, outside the selected group",
            "highest-request group on each CCD, ties included",
        ] {
            assert!(help.contains(said), "help lost {said:?}: {help}");
        }
        for gone in [
            "The first full cycle",
            "on its own account",
            "shallower than its failing offset",
            "setting the shared core voltage",
        ] {
            assert!(!help.contains(gone), "help still says {gone:?}: {help}");
        }
    }

    #[test]
    fn help_links_the_tuner_spec_and_states_no_rule() {
        let help = words(Quit::Keys);
        assert!(
            help.contains("https://github.com/shgew/togi/blob/main/docs/spec/tuner.md"),
            "help lost the tuner specification link: {help}"
        );
        for rule in [
            "when the part starts",
            "voltage-targeted counts",
            "halfway",
            "then both",
            "one count shallower",
            "one count toward",
            "one count deeper",
            "Its pass confirms",
            "as far as its failure points",
            "most rounds left",
            "if none fails",
            "then one confirmation cycle",
            "passed full cycle confirms",
            "after the last passed",
            "held failure",
            "hold expires",
            "second failure",
            "still valid",
            "repeats until stopped",
            "Unresolved",
            "no smaller group",
            "-50, or",
            "past search",
            "by offset when offsets",
            "stand in for requests",
            "not a guarantee",
            "evidence, not",
            "until every core has one",
        ] {
            assert!(
                !help.contains(rule),
                "help restates the tuning rule {rule:?}"
            );
        }
    }

    #[test_case(115; "115 columns")]
    #[test_case(235; "235 columns")]
    fn the_full_help_renders_unscrolled_and_ends_on_text(width: usize) {
        let (body, scroll) = help_body(width, 1000, 0, C, Quit::Keys);
        assert!(
            scroll == 0 && !body.is_empty(),
            "{width} columns: full help was not rendered"
        );
        assert!(
            !plain(&body[body.len() - 1]).trim().is_empty(),
            "{width} columns: full help ends on a blank content row"
        );
    }

    #[test]
    fn help_under_three_columns_keeps_the_state_explanations_whole() {
        let width = 115;
        let (body, _) = help_body(width, 1000, 0, C, Quit::Keys);
        let words = plain_rows(&body)
            .iter()
            .map(|row| row.chars().skip(2).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        let one: String = words.split_whitespace().collect::<Vec<_>>().join(" ");
        for explanation in [
            "SEARCH finding its solo limit with light and heavy-vector trials alone",
            "CONFIRM checking its candidate solo limit with repeated light and heavy-vector trials alone",
            "FOUND solo limit checked; waiting at 0 WAITING",
            "WAITING at 0, waiting for its first search turn",
            "MEMBER kept with its hunt group at its failing offset; another member may be probed",
        ] {
            assert!(
                one.contains(explanation),
                "{width} columns: help lost state explanation {explanation:?}"
            );
        }
    }

    #[test]
    fn narrow_help_scrolls_one_column_through_every_section() {
        let (top, scroll) = help_body(77, 19, 0, C, Quit::Keys);
        let (end, end_scroll) = help_body(77, 19, usize::MAX, C, Quit::Keys);
        let (clamped, _) = help_body(77, 19, scroll + 100, C, Quit::Keys);
        assert!(
            scroll != 0 && end_scroll == scroll,
            "narrow help must scroll: top={scroll} end={end_scroll}"
        );
        assert_eq!(plain_rows(&end), plain_rows(&clamped), "scroll beyond end");
        assert!(
            plain(&top[0]).contains("NAVIGATION"),
            "help does not open at the first section: {:?}",
            plain(&top[0])
        );
        let text = plain_rows(&end).join("\n");
        assert!(
            text.contains("KINDS OF LOAD") && text.contains("R7 all-core"),
            "help end loses the last section: {text}"
        );
        assert!(
            !plain(&top[0]).contains("THE PAGES"),
            "narrow help squeezes columns together"
        );
        assert!(
            !chars_from(&plain(&end[end.len() - 1]), 2).trim().is_empty(),
            "narrow help ends on a blank content row"
        );
        for lines in [&top, &end] {
            assert!(
                lines.len() <= 19,
                "help exceeds its row allocation: {}",
                lines.len()
            );
            for line in lines {
                assert!(
                    line_width(line) <= 77,
                    "help exceeds its column allocation: {:?}",
                    plain(line)
                );
            }
        }
        let mut all = Vec::new();
        let (_, extent) = help_body(77, 19, 0, C, Quit::Keys);
        for at in 0..=extent {
            all.extend(plain_rows(&help_body(77, 19, at, C, Quit::Keys).0));
        }
        let all = all.join("\n");
        for title in TITLES {
            assert!(all.contains(title), "scrolling never reaches {title:?}");
        }
    }

    #[test]
    fn help_bodies_fit_small_rectangles() {
        for width in [0, 1, 2, 10, 77, 155, 194, 195, 196, 197, 235] {
            for height in [0, 1, 19, 61] {
                for scroll in [0, 7, usize::MAX] {
                    let (body, _) = help_body(width, height, scroll, C, Quit::Keys);
                    assert!(
                        body.len() <= height,
                        "{width}x{height}: body has {} rows",
                        body.len()
                    );
                    for line in &body {
                        assert!(
                            line_width(line) <= width,
                            "{width}x{height}: body writes beyond rectangle: {:?}",
                            plain(line)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_title_rule_keeps_whole_words_and_a_dash_track() {
        let text = |title, width| plain(&title_rule(title, width, C));
        assert_eq!(text("SOLO LIMITS", 11), "SOLO LIMITS");
        assert_eq!(text("SOLO LIMITS", 14), "SOLO LIMITS ──");
        assert_eq!(text("SOLO LIMITS", 8), "SOLO... ");
        assert_eq!(text("SOLO LIMITS", 0), "");
        assert_eq!(text("REQUESTERS", 8), " ───────");
        for width in 0..30 {
            assert!(line_width(&title_rule("SOLO LIMITS", width, C)) <= width);
        }
    }

    #[test_case(&[("requester", "evidence")], 20, 6, ""; "a label too long for its column is dropped")]
    #[test_case(&[("top requester", "evidence")], 30, 10, "top..."; "a label is cut at a word")]
    #[test_case(&[("top requester", "evidence")], 60, 15, "top requester"; "a label that fits stays whole")]
    fn help_labels_keep_whole_words(
        items: &[(&'static str, &'static str)],
        width: usize,
        label_width: usize,
        want: &str,
    ) {
        let section = Section {
            title: "WORDS",
            items,
        };
        let lines = section_lines(&section, width, false, C);
        let row = plain(&lines[2]);
        assert_eq!(
            row[..label_width].trim(),
            want,
            "width {width}: help label must fit by whole words"
        );
        assert_eq!(
            row[label_width..].trim(),
            "evidence",
            "width {width}: fitted label lost its text gap: {row:?}"
        );
    }

    #[test_case(&TUNING; "tuning")]
    #[test_case(&LOAD; "load")]
    fn a_section_ends_on_text_not_padding(section: &Section) {
        for width in [40, 70, 115] {
            let lines = section_lines(section, width, section.title == TUNING.title, C);
            assert!(
                !plain(&lines[lines.len() - 1]).trim().is_empty(),
                "{:?} at {width} ends on blank padding",
                section.title
            );
        }
    }

    #[test]
    fn help_names_the_exit_key_only_where_it_works() {
        let standalone = words(Quit::Keys);
        assert!(standalone.contains("q, Ctrl-C Exit the dashboard; tuning itself keeps running."));
        assert!(!standalone.contains("no exit key"), "{standalone}");
        let boot = words(Quit::Ignored);
        assert!(
            boot.contains(
                "exit This dashboard has no exit key: only stopping its service ends it. Tuning itself keeps running."
            ),
            "{boot}"
        );
        for gone in [
            "exit dashboard",
            "Exit the dashboard",
            "q, Ctrl-C",
            "Ctrl-D",
        ] {
            assert!(
                !boot.contains(gone),
                "tuning-boot help says {gone:?}: {boot}"
            );
        }
        assert!(
            boot.contains("last observed"),
            "the rest of the top line stays"
        );
        for width in [77, 115, 235] {
            for height in [19, 1000] {
                let (body, _) = help_body(width, height, usize::MAX, C, Quit::Ignored);
                assert!(body.len() <= height);
            }
        }
    }

    #[test]
    fn the_help_view_draws_inside_its_body_and_reports_its_extent() {
        let sc = screen(View::Help, 80, 24, C, true);
        let layout = Layout::new(&sc);
        let (buf, drawn) = render_missing(&sc);
        let (_, extent) = help_body(
            usize::from(layout.body.width),
            body_rows(&sc),
            0,
            C,
            Quit::Keys,
        );
        assert_eq!(
            drawn,
            Drawn {
                scroll: extent,
                hold: Hold::Never
            }
        );
        let rows = rows_of(&buf);
        assert!(rows[usize::from(layout.header.y)].contains("no session yet"));
        assert!(rows.iter().any(|row| row.contains("NAVIGATION")));
        assert!(rows[usize::from(layout.footer.y)].contains("9/9 Help"));
    }

    #[test_case(80, 24; "80 by 24")]
    #[test_case(160, 45; "160 by 45")]
    #[test_case(240, 67; "240 by 67")]
    #[test_case(240, 30; "240 by 30")]
    fn a_scroll_at_or_past_the_extent_follows_the_end(width: u16, height: u16) {
        let mut sc = screen(View::Help, width, height, C, true);
        sc.scroll = usize::MAX;
        let (end, drawn) = render_missing(&sc);
        for scroll in [drawn.scroll, drawn.scroll + 1, 100_000] {
            sc.scroll = scroll;
            assert_eq!(
                render_missing(&sc).0,
                end,
                "{width}x{height} scroll {scroll}"
            );
        }
        sc.scroll = drawn.scroll.saturating_sub(1);
        if drawn.scroll > 0 {
            assert_ne!(
                render_missing(&sc).0,
                end,
                "{width}x{height}: one line above the end is not the end"
            );
        }
    }

    #[test_case(240, 67; "240 by 67")]
    #[test_case(160, 45; "160 by 45")]
    #[test_case(120, 33; "120 by 33")]
    #[test_case(80, 24; "80 by 24")]
    fn help_stays_in_bounds_and_draws_only_console_glyphs_at_the_size(width: u16, height: u16) {
        for colours in [Colours::Full, Colours::Console, Colours::None] {
            for quit in [Quit::Keys, Quit::Ignored] {
                for scroll in [0, 3, usize::MAX, 100_000] {
                    for keys in [true, false] {
                        let mut sc = screen(View::Help, width, height, colours, keys);
                        sc.scroll = scroll;
                        sc.quit = quit;
                        let (buf, _) = render_missing(&sc);
                        assert_frame_bounds(&buf, &sc);
                        assert_glyphs_drawable(&buf, &format!("help {width}x{height}"));
                    }
                }
            }
        }
    }

    #[test]
    fn help_stays_in_bounds_at_every_tiny_size() {
        for width in 0..=80 {
            for height in [0, 1, 2, 3, 4, 5, 8, 24] {
                let sc = screen(View::Help, width, height, Colours::Full, true);
                let (buf, _) = render_missing(&sc);
                assert_frame_bounds(&buf, &sc);
            }
        }
    }
}
