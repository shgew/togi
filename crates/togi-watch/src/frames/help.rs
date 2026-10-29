//! The help view: static explanations of the screen, in three columns when wide and one scrolling column
//! otherwise.
//!
//! Labels and examples keep whole words and values at every width: a help
//! example's gauge is clipped by cells first, and its night edge, state
//! word or offset are left out whole, never cut. The view shows a scrollbar
//! whenever its contents need scrolling, and scrolling to the end reaches the
//! last text row, not trailing blank rows.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::colours::Paint;
use super::dash::marks;
use super::fit::{cut_words, display_width, line_width, truncate_styled, wrap_styled};
use super::scroll::scroll_body_with_bar;
use crate::screen::{Colours, Quit};

struct Section<'a> {
    title: &'static str,
    items: &'a [(&'static str, &'static str)],
}

const SCREEN_TITLE: &str = "READING THE SCREEN";

/// The rows of the reading legend. Applied depth and the night edge share the profile's bar glyph, told apart
/// by colour.
fn screen_items(colours: Colours) -> Vec<(&'static str, &'static str)> {
    let rich = colours == Colours::Rich;
    let pick = |console, rich_| if rich { rich_ } else { console };
    vec![
        ("►", "the core carries load in this trial"),
        (
            pick("▄▄▄", "▆▆▆"),
            "offset applied now, or saved after a stop restored the hardware; white when this trial loads the core",
        ),
        ("?", "no hardware offset has been read back for this core"),
        (
            pick("grey ▄▄▄", "grey ▆▆▆"),
            "the same depth, grey when the core is not loaded",
        ),
        (
            "···",
            "the hard night ceiling C for this tuning unit, not statistical permission",
        ),
        (
            pick("red ▄", "red ▆"),
            "the night edge U of its tuning unit: the shallowest offset that failed in isolation, one count wide at Wide and Medium",
        ),
        (
            pick("amber ▄", "amber ▆"),
            "a censored edge: nothing failed down to the mapping limit",
        ),
        ("cyan", "running now"),
        (
            "amber",
            "a hold, a veto, a retreat, a crash, or outstanding qualification",
        ),
        ("red", "failures, past and present"),
        ("green", "passed, done, covered"),
        ("grey", "idle, still to come, unknown"),
    ]
}

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

/// The top-line row that names the quit key, left out where the keys are ignored.
const EXIT: (&str, &str) = ("q", "exit dashboard; tuning itself keeps running");

const TUNING: Section = Section {
    title: "WHAT TOGI DOES",
    items: &[
        (
            "NIGHT",
            "Until you end it, togi maps each unit's edge U, sweeps uncovered cells from U toward C, qualifies exact profiles, reproduces daily events and runs controls. Idle time repeats exact-current-D qualification cells, never S-directed probes.",
        ),
        (
            "DAY",
            "permission: experimental hold + exact night qualification. The day panel shows the exact-profile hold, the last step, allowance and what blocks the next one. Finish ends exploration, not observation or safety work.",
        ),
        (
            "CCD TABLES",
            "Physical CCD tables keep the topology visible. Each tuning unit shows applied D, hard ceiling C and tagged U; core rows retain their hardware readback. S is diagnostic only and never authorizes a step.",
        ),
        (
            "R6",
            "During idle trials this screen holds still, clock included, so it cannot wake the cores.",
        ),
        (
            "NO ETA",
            "Nights and days follow the clock, but when a hold completes depends on what runs, so togi shows only what is scheduled and the time left in the current trial.",
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
            "offset",
            "Curve Optimizer counts, 0 to -50; deeper is more negative",
        ),
        ("trial", "one launch of one workload on its target"),
        (
            "D",
            "the tuning unit's applied day offset; saved and proposed profiles are not hardware readback",
        ),
        (
            "C",
            "the hard night ceiling, with its edge and margin witnesses; unknown U blocks ordinary promotion",
        ),
        (
            "S",
            "non-authorizing statistical diagnostic, with its slope, witnesses and gaps; never permission or a retreat trigger",
        ),
        (
            "U",
            "the night edge: the shallowest offset that failed in isolation; unknown until mapped, censored when nothing failed down to the limit",
        ),
        (
            "slope",
            "how fast a class's failure rate grows with depth: a fallback until enough is measured",
        ),
        (
            "bound by",
            "what limits diagnostic S: a class's failure rate, edge margin, or baseline; not what permits D",
        ),
        (
            "covered",
            "a class with diagnostic exposure; this does not qualify a whole profile",
        ),
        (
            "hold",
            "clean observed hours at the current exact D; earlier profiles, nights and paused time do not count",
        ),
        (
            "step",
            "hold, promote one tuning unit, retreat, restore, or reset after a firmware change",
        ),
        ("veto", "a profile an event forbids until the veto clears"),
        (
            "freeze",
            "keeps D and blocks promotions; a required retreat or firmware reset still applies",
        ),
        (
            "qualifying",
            "running a whole profile until every class has witnessed it",
        ),
        (
            "reproducing",
            "re-running a daily event's conditions to see whether it comes back",
        ),
        (
            "share",
            "the part of the night's elapsed time a queue has used, against the part it is meant to take",
        ),
        (
            "allowance",
            "machine-wide consumed and remaining events, including provisional charges; reclassification does not refund them and finish does not hide later events",
        ),
        (
            "experimental",
            "experimental: 50 clean hours; at most 1 crash per 17 h (95%, fixed-rate model). This is an example for current exact-profile evidence, not a reliability certificate; finish alone never meets the hazard goal",
        ),
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
    let title = cut_words(title, width, colours);
    let used = display_width(&title, colours);
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
        .map(|&(label, _)| display_width(label, colours))
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
            colours,
        );
        for (row, line) in wrapped.into_iter().enumerate() {
            let mut spans = if row == 0 {
                let name = cut_words(label, label_width.saturating_sub(1), colours);
                let pad = label_width.saturating_sub(display_width(&name, colours));
                vec![Paint::White.span(colours, name), Span::raw(" ".repeat(pad))]
            } else {
                vec![Span::raw(" ".repeat(label_width))]
            };
            spans.extend(line.spans);
            out.push(truncate_styled(&Line::from(spans), width, colours));
        }
        if spaced && i + 1 < section.items.len() {
            out.push(blank());
        }
    }
    out
}
/// A help example keeps the text of its row whole: its heads run from the full core, offset and state to
/// nothing, each with its night edge or without, and the first that fits wins. Only the gauge is clipped by
/// cells.
fn example_row(
    width: usize,
    heads: &[String],
    gauge: &Line<'static>,
    edge: Option<(&str, Paint)>,
    colours: Colours,
) -> Line<'static> {
    for head in heads {
        for with_edge in [edge.is_some(), false] {
            let head_width = display_width(head, colours);
            let used = head_width
                + match edge {
                    Some((text, _)) if with_edge => 2 + display_width(text, colours),
                    Some(_) | None => 0,
                };
            if used > width {
                continue;
            }
            let mut spans = Vec::new();
            if !head.is_empty() {
                spans.push(Paint::White.span(colours, head.clone()));
            }
            spans.extend(gauge.spans.iter().cloned());
            let mut line = truncate_styled(&Line::from(spans), width - used + head_width, colours);
            if let Some((text, paint)) = edge.filter(|_| with_edge) {
                line.spans.push(Span::raw("  "));
                line.spans.push(paint.span(colours, text.to_owned()));
            }
            return line;
        }
    }
    Line::default()
}

fn gauge(colours: Colours, parts: &[(Paint, &str, usize)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = parts
        .iter()
        .map(|&(tone, glyph, count)| tone.span(colours, glyph.repeat(count)))
        .collect();
    let used: usize = parts
        .iter()
        .map(|&(_, glyph, count)| display_width(glyph, colours) * count)
        .sum();
    spans.push(Span::raw(" ".repeat(50usize.saturating_sub(used))));
    Line::from(spans)
}

/// One worked example of a core's row.
struct Example {
    marker: &'static str,
    core: u32,
    /// The offset read back; `None` shows `?`.
    offset: Option<i32>,
    state: &'static str,
    edge: Option<(&'static str, Paint)>,
    note: &'static str,
    gauge: Line<'static>,
}

/// The reading examples: a loaded core under a known edge, an idle core whose CCD has no edge yet, and a core
/// with no readback under a censored edge.
fn examples(colours: Colours) -> [Example; 3] {
    let marks = marks(colours);
    let (applied, edge) = (marks.applied.to_string(), marks.edge.to_string());
    [
        Example {
            marker: "► ",
            core: 4,
            offset: Some(-12),
            state: "LOADED",
            edge: Some(("-31", Paint::Bad)),
            note: "applied -12 under load; dots reach hard ceiling C at -26; U observed at -31",
            gauge: gauge(
                colours,
                &[
                    (Paint::White, &applied, 12),
                    (Paint::Grey, "·", 14),
                    (Paint::Text, " ", 4),
                    (Paint::Bad, &edge, 1),
                ],
            ),
        },
        Example {
            marker: "  ",
            core: 9,
            offset: Some(-8),
            state: "",
            edge: None,
            note: "idle, so grey; C is unknown until U is mapped, so there are no ceiling dots or edge mark",
            gauge: gauge(colours, &[(Paint::Grey, &applied, 8)]),
        },
        Example {
            marker: "  ",
            core: 11,
            offset: None,
            state: "",
            edge: Some(("-50", Paint::Warn)),
            note: "no readback yet; U censored at -50 gives hard ceiling C at -45, not physical -50",
            gauge: gauge(
                colours,
                &[
                    (Paint::Grey, "·", 45),
                    (Paint::Text, " ", 4),
                    (Paint::Warn, &edge, 1),
                ],
            ),
        },
    ]
}

fn reading(width: usize, colours: Colours) -> Vec<Line<'static>> {
    let mut out = vec![title_rule(SCREEN_TITLE, width, colours), blank()];
    for example in examples(colours) {
        let Example {
            marker,
            core,
            offset,
            state,
            ..
        } = example;
        let shown = offset.map_or_else(|| "  ?".to_owned(), |offset| format!("{offset:3}"));
        let prefix = format!("{marker}{core:02}  {shown}  {state:<8} ");
        let heads = [
            prefix.clone(),
            format!("{marker}{core:02}  {shown} "),
            format!("{marker}{core:02} "),
            String::new(),
        ];
        out.push(example_row(
            width,
            &heads,
            &example.gauge,
            example.edge,
            colours,
        ));
        let indent = display_width(&prefix, colours).min(width.saturating_sub(20));
        for line in wrap_styled(
            example.note,
            width.saturating_sub(indent).max(1),
            Paint::Grey.style(colours),
            colours,
        ) {
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            spans.extend(line.spans);
            out.push(Line::from(spans));
        }
        out.push(blank());
    }
    let screen_items = screen_items(colours);
    out.extend(
        section_lines(
            &Section {
                title: SCREEN_TITLE,
                items: &screen_items,
            },
            width,
            false,
            colours,
        )
        .into_iter()
        .skip(2),
    );
    out
}

/// The help body in `width` cells and `height` rows, scrolled `scroll` lines down, with its scrollbar in the
/// first two cells of every row; and how far it can scroll. A scroll at or past that extent shows the end.
#[must_use]
pub fn help_body(
    width: usize,
    height: usize,
    scroll: usize,
    colours: Colours,
    quit: Quit,
) -> (Vec<Line<'static>>, usize) {
    let top_items: Vec<_> = TOP
        .items
        .iter()
        .copied()
        .chain((quit == Quit::Keys).then_some(EXIT))
        .collect();
    let top = Section {
        title: TOP.title,
        items: &top_items,
    };
    let body_width = width.saturating_sub(2).max(1);
    let lines = if width >= THREE_COLUMNS {
        let usable = body_width - 8;
        let left = usable * 81 / 227;
        let middle = usable * 75 / 227;
        let right = usable - left - middle;
        let columns = [
            [
                reading(left, colours),
                vec![blank(), blank()],
                section_lines(&top, left, false, colours),
            ]
            .concat(),
            section_lines(&TUNING, middle, true, colours),
            [
                section_lines(&WORDS, right, false, colours),
                vec![blank(), blank()],
                section_lines(&LOAD, right, false, colours),
            ]
            .concat(),
        ];
        let widths = [left, middle, right];
        let count = columns.iter().map(Vec::len).max().unwrap_or(0);
        (0..count)
            .map(|row| {
                let mut spans = Vec::new();
                for (col, column) in columns.iter().enumerate() {
                    let cell = column.get(row);
                    let used = cell.map_or(0, |cell| line_width(cell, colours));
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
        let mut lines = reading(body_width, colours);
        for section in [&top, &TUNING, &WORDS, &LOAD] {
            lines.extend([blank(), blank()]);
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
    use super::{
        LOAD, Section, THREE_COLUMNS, TOP, TUNING, help_body, reading, section_lines, title_rule,
    };
    use crate::frames::fit::{line_width, plain};
    use crate::frames::layout::Layout;
    use crate::frames::testing::{
        assert_frame_bounds, assert_glyphs_drawable, render_missing, rows_of, screen,
    };
    use crate::screen::{Colours, Drawn, Hold, Quit, View};
    use test_case::test_case;

    const C: Colours = Colours::Full;

    fn plain_rows(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines.iter().map(|line| plain(line, C)).collect()
    }

    fn plain_rows_in(lines: &[ratatui::text::Line<'static>], colours: Colours) -> Vec<String> {
        lines.iter().map(|line| plain(line, colours)).collect()
    }

    fn chars_from(text: &str, from: usize) -> String {
        text.chars().skip(from).collect()
    }

    fn first_char(text: &str) -> char {
        text.chars().next().unwrap_or(' ')
    }

    #[test]
    fn wide_help_uses_three_columns() {
        let (body, scroll) = help_body(235, 61, 0, C, Quit::Keys);
        assert_eq!(scroll, 0, "wide help unexpectedly needs scrolling");
        let rows = plain_rows(&body);
        for title in ["READING THE SCREEN", "WHAT TOGI DOES", "WORDS"] {
            assert!(
                rows[0].contains(title),
                "wide help lost column {title:?}: {:?}",
                rows[0]
            );
        }
        let text = rows.join("\n");
        for title in ["THE TOP LINE", "KINDS OF LOAD", "NO ETA", "CCD TABLES"] {
            assert!(text.contains(title), "wide help lost {title:?}");
        }
        for (row, line) in body.iter().enumerate() {
            assert!(
                line_width(line, C) <= 235,
                "row {row} exceeds help rectangle"
            );
        }
        assert!(
            !rows[rows.len() - 1].trim().is_empty(),
            "wide help ends on a blank content row"
        );
    }

    #[test_case::test_case(&TOP; "top")]
    #[test_case::test_case(&TUNING; "tuning")]
    #[test_case::test_case(&LOAD; "load")]
    fn a_wide_help_section_has_no_trailing_padding(section: &Section<'_>) {
        let lines = section_lines(section, 70, section.title == TUNING.title, C);
        assert!(
            !plain(&lines[lines.len() - 1], C).trim().is_empty(),
            "wide column's final section {:?} ends on blank padding",
            section.title
        );
    }

    #[test_case(195; "195 columns")]
    #[test_case(235; "235 columns")]
    fn short_wide_help_scrolls_and_keeps_its_scrollbar(width: usize) {
        let (top, scroll) = help_body(width, 19, 0, C, Quit::Keys);
        let (end, end_scroll) = help_body(width, 19, usize::MAX, C, Quit::Keys);
        let (clamped, _) = help_body(width, 19, scroll + 100, C, Quit::Keys);
        assert!(
            scroll != 0 && end_scroll == scroll,
            "short wide help must scroll: top={scroll} end={end_scroll}"
        );
        assert_eq!(plain_rows(&end), plain_rows(&clamped), "scroll beyond end");
        for title in ["READING THE SCREEN", "WHAT TOGI DOES", "WORDS"] {
            assert!(
                plain(&top[0], C).contains(title),
                "short wide help lost column {title:?}"
            );
        }
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
                    line_width(line, C) <= width,
                    "{name} row {row} exceeds the help rectangle"
                );
                let text = plain(line, C);
                assert_eq!(
                    text.chars().nth(1),
                    Some(' '),
                    "{name} row {row} loses the scrollbar's two columns: {text:?}"
                );
                got.push(first_char(&text));
            }
            assert_eq!(got, bar, "{name} scrollbar");
        }
        let last = plain(&end[end.len() - 1], C);
        assert!(
            !chars_from(&last, 2).trim().is_empty(),
            "short wide help ends on a blank content row"
        );
    }

    #[test]
    fn help_glosses_no_record_only_trial() {
        let (help, _) = help_body(115, 1000, 0, C, Quit::Keys);
        let words = plain_rows(&help)
            .join("\n")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for gone in ["record only", "record-only"] {
            assert!(!words.contains(gone), "help glosses {gone:?}: {words}");
        }
        assert!(
            words.contains("changed what togi does; skips do not count"),
            "help lost the top-line note: {words}"
        );
    }

    #[test]
    fn help_words_claim_no_more_than_the_journal_shows() {
        let (help, _) = help_body(115, 1000, 0, C, Quit::Keys);
        let words = plain_rows(&help)
            .join("\n")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for said in [
            "the night edge U of its tuning unit: the shallowest offset that failed in isolation",
            "the night edge: the shallowest offset that failed in isolation",
            "non-authorizing statistical diagnostic",
            "permission: experimental hold + exact night qualification",
            "clean observed hours at the current exact D",
        ] {
            assert!(words.contains(said), "help lost {said:?}: {words}");
        }
        for gone in [
            "SOLO LIMITS",
            "PHASE 1",
            "PHASE 2",
            "CLEAN CYCLES",
            "BIOS PROFILE",
            "solo limit",
            "failure point",
            "combination",
            "top requester",
            "self-sufficient",
            "hunt",
            "parked",
        ] {
            assert!(!words.contains(gone), "help still says {gone:?}: {words}");
        }
    }

    #[test]
    fn help_links_the_tuner_spec_and_states_no_rule() {
        let (help, _) = help_body(115, 1000, 0, C, Quit::Keys);
        let words = plain_rows(&help)
            .join("\n")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            words.contains("https://github.com/shgew/togi/blob/main/docs/spec/tuner.md"),
            "help lost the tuner specification link: {words}"
        );
        for rule in [
            "request order",
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
                !words.contains(rule),
                "help restates the tuning rule {rule:?}"
            );
        }
    }

    #[test_case(115; "115 columns")]
    #[test_case(235; "235 columns")]
    fn full_help_renders_whole_and_explains_the_words(width: usize) {
        let (body, scroll) = help_body(width, 1000, 0, C, Quit::Keys);
        assert!(
            scroll == 0 && !body.is_empty(),
            "{width} columns: full help was not rendered"
        );
        let column = if width >= THREE_COLUMNS {
            let first = plain(&body[0], C);
            let prefix = first
                .split_once("WORDS")
                .unwrap_or_else(|| panic!("wide help lost the words column: {first}"))
                .0;
            prefix.chars().count()
        } else {
            0
        };
        let lines: Vec<String> = body
            .iter()
            .enumerate()
            .map(|(row, line)| {
                assert!(
                    line_width(line, C) <= width,
                    "{width} columns: full help row {row} exceeds its rectangle"
                );
                chars_from(&plain(line, C), column)
            })
            .collect();
        let words = lines
            .join("\n")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for explanation in [
            "D the tuning unit's applied day offset; saved and proposed profiles are not hardware readback",
            "S non-authorizing statistical diagnostic",
            "U the night edge: the shallowest offset that failed in isolation; unknown until mapped, censored when nothing failed down to the limit",
            "covered a class with diagnostic exposure",
            "veto a profile an event forbids until the veto clears",
        ] {
            assert!(
                words.contains(explanation),
                "{width} columns: help lost explanation {explanation:?}"
            );
        }
        assert!(
            !plain(&body[body.len() - 1], C).trim().is_empty(),
            "{width} columns: full help ends on a blank content row"
        );
    }

    #[test]
    fn narrow_help_scrolls_one_column() {
        let (top, scroll) = help_body(77, 19, 0, C, Quit::Keys);
        let (end, end_scroll) = help_body(77, 19, usize::MAX, C, Quit::Keys);
        let (clamped, _) = help_body(77, 19, scroll + 100, C, Quit::Keys);
        assert!(
            scroll != 0 && end_scroll == scroll,
            "narrow help must scroll: top={scroll} end={end_scroll}"
        );
        assert_eq!(plain_rows(&end), plain_rows(&clamped), "scroll beyond end");
        assert!(
            plain(&top[0], C).contains("READING THE SCREEN"),
            "help does not open at the first section: {:?}",
            plain(&top[0], C)
        );
        let text = plain_rows(&end).join("\n");
        assert!(
            text.contains("KINDS OF LOAD") && text.contains("R7 all-core"),
            "help end loses the last section: {text}"
        );
        assert!(
            !plain(&top[0], C).contains("WHAT TOGI DOES"),
            "narrow help squeezes columns together"
        );
        assert!(
            !chars_from(&plain(&end[end.len() - 1], C), 2)
                .trim()
                .is_empty(),
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
                    line_width(line, C) <= 77,
                    "help exceeds its column allocation: {:?}",
                    plain(line, C)
                );
            }
        }
    }

    #[test_case::test_case(0, 0, 0; "w0 h0 scroll0")]
    #[test_case::test_case(0, 0, 7; "w0 h0 scroll1")]
    #[test_case::test_case(0, 0, usize::MAX; "w0 h0 scroll2")]
    #[test_case::test_case(0, 1, 0; "w0 h1 scroll0")]
    #[test_case::test_case(0, 1, 7; "w0 h1 scroll1")]
    #[test_case::test_case(0, 1, usize::MAX; "w0 h1 scroll2")]
    #[test_case::test_case(0, 19, 0; "w0 h19 scroll0")]
    #[test_case::test_case(0, 19, 7; "w0 h19 scroll1")]
    #[test_case::test_case(0, 19, usize::MAX; "w0 h19 scroll2")]
    #[test_case::test_case(0, 61, 0; "w0 h61 scroll0")]
    #[test_case::test_case(0, 61, 7; "w0 h61 scroll1")]
    #[test_case::test_case(0, 61, usize::MAX; "w0 h61 scroll2")]
    #[test_case::test_case(1, 0, 0; "w1 h0 scroll0")]
    #[test_case::test_case(1, 0, 7; "w1 h0 scroll1")]
    #[test_case::test_case(1, 0, usize::MAX; "w1 h0 scroll2")]
    #[test_case::test_case(1, 1, 0; "w1 h1 scroll0")]
    #[test_case::test_case(1, 1, 7; "w1 h1 scroll1")]
    #[test_case::test_case(1, 1, usize::MAX; "w1 h1 scroll2")]
    #[test_case::test_case(1, 19, 0; "w1 h19 scroll0")]
    #[test_case::test_case(1, 19, 7; "w1 h19 scroll1")]
    #[test_case::test_case(1, 19, usize::MAX; "w1 h19 scroll2")]
    #[test_case::test_case(1, 61, 0; "w1 h61 scroll0")]
    #[test_case::test_case(1, 61, 7; "w1 h61 scroll1")]
    #[test_case::test_case(1, 61, usize::MAX; "w1 h61 scroll2")]
    #[test_case::test_case(2, 0, 0; "w2 h0 scroll0")]
    #[test_case::test_case(2, 0, 7; "w2 h0 scroll1")]
    #[test_case::test_case(2, 0, usize::MAX; "w2 h0 scroll2")]
    #[test_case::test_case(2, 1, 0; "w2 h1 scroll0")]
    #[test_case::test_case(2, 1, 7; "w2 h1 scroll1")]
    #[test_case::test_case(2, 1, usize::MAX; "w2 h1 scroll2")]
    #[test_case::test_case(2, 19, 0; "w2 h19 scroll0")]
    #[test_case::test_case(2, 19, 7; "w2 h19 scroll1")]
    #[test_case::test_case(2, 19, usize::MAX; "w2 h19 scroll2")]
    #[test_case::test_case(2, 61, 0; "w2 h61 scroll0")]
    #[test_case::test_case(2, 61, 7; "w2 h61 scroll1")]
    #[test_case::test_case(2, 61, usize::MAX; "w2 h61 scroll2")]
    #[test_case::test_case(10, 0, 0; "w10 h0 scroll0")]
    #[test_case::test_case(10, 0, 7; "w10 h0 scroll1")]
    #[test_case::test_case(10, 0, usize::MAX; "w10 h0 scroll2")]
    #[test_case::test_case(10, 1, 0; "w10 h1 scroll0")]
    #[test_case::test_case(10, 1, 7; "w10 h1 scroll1")]
    #[test_case::test_case(10, 1, usize::MAX; "w10 h1 scroll2")]
    #[test_case::test_case(10, 19, 0; "w10 h19 scroll0")]
    #[test_case::test_case(10, 19, 7; "w10 h19 scroll1")]
    #[test_case::test_case(10, 19, usize::MAX; "w10 h19 scroll2")]
    #[test_case::test_case(10, 61, 0; "w10 h61 scroll0")]
    #[test_case::test_case(10, 61, 7; "w10 h61 scroll1")]
    #[test_case::test_case(10, 61, usize::MAX; "w10 h61 scroll2")]
    #[test_case::test_case(77, 0, 0; "w77 h0 scroll0")]
    #[test_case::test_case(77, 0, 7; "w77 h0 scroll1")]
    #[test_case::test_case(77, 0, usize::MAX; "w77 h0 scroll2")]
    #[test_case::test_case(77, 1, 0; "w77 h1 scroll0")]
    #[test_case::test_case(77, 1, 7; "w77 h1 scroll1")]
    #[test_case::test_case(77, 1, usize::MAX; "w77 h1 scroll2")]
    #[test_case::test_case(77, 19, 0; "w77 h19 scroll0")]
    #[test_case::test_case(77, 19, 7; "w77 h19 scroll1")]
    #[test_case::test_case(77, 19, usize::MAX; "w77 h19 scroll2")]
    #[test_case::test_case(77, 61, 0; "w77 h61 scroll0")]
    #[test_case::test_case(77, 61, 7; "w77 h61 scroll1")]
    #[test_case::test_case(77, 61, usize::MAX; "w77 h61 scroll2")]
    #[test_case::test_case(155, 0, 0; "w155 h0 scroll0")]
    #[test_case::test_case(155, 0, 7; "w155 h0 scroll1")]
    #[test_case::test_case(155, 0, usize::MAX; "w155 h0 scroll2")]
    #[test_case::test_case(155, 1, 0; "w155 h1 scroll0")]
    #[test_case::test_case(155, 1, 7; "w155 h1 scroll1")]
    #[test_case::test_case(155, 1, usize::MAX; "w155 h1 scroll2")]
    #[test_case::test_case(155, 19, 0; "w155 h19 scroll0")]
    #[test_case::test_case(155, 19, 7; "w155 h19 scroll1")]
    #[test_case::test_case(155, 19, usize::MAX; "w155 h19 scroll2")]
    #[test_case::test_case(155, 61, 0; "w155 h61 scroll0")]
    #[test_case::test_case(155, 61, 7; "w155 h61 scroll1")]
    #[test_case::test_case(155, 61, usize::MAX; "w155 h61 scroll2")]
    #[test_case::test_case(194, 0, 0; "w194 h0 scroll0")]
    #[test_case::test_case(194, 0, 7; "w194 h0 scroll1")]
    #[test_case::test_case(194, 0, usize::MAX; "w194 h0 scroll2")]
    #[test_case::test_case(194, 1, 0; "w194 h1 scroll0")]
    #[test_case::test_case(194, 1, 7; "w194 h1 scroll1")]
    #[test_case::test_case(194, 1, usize::MAX; "w194 h1 scroll2")]
    #[test_case::test_case(194, 19, 0; "w194 h19 scroll0")]
    #[test_case::test_case(194, 19, 7; "w194 h19 scroll1")]
    #[test_case::test_case(194, 19, usize::MAX; "w194 h19 scroll2")]
    #[test_case::test_case(194, 61, 0; "w194 h61 scroll0")]
    #[test_case::test_case(194, 61, 7; "w194 h61 scroll1")]
    #[test_case::test_case(194, 61, usize::MAX; "w194 h61 scroll2")]
    #[test_case::test_case(195, 0, 0; "w195 h0 scroll0")]
    #[test_case::test_case(195, 0, 7; "w195 h0 scroll1")]
    #[test_case::test_case(195, 0, usize::MAX; "w195 h0 scroll2")]
    #[test_case::test_case(195, 1, 0; "w195 h1 scroll0")]
    #[test_case::test_case(195, 1, 7; "w195 h1 scroll1")]
    #[test_case::test_case(195, 1, usize::MAX; "w195 h1 scroll2")]
    #[test_case::test_case(195, 19, 0; "w195 h19 scroll0")]
    #[test_case::test_case(195, 19, 7; "w195 h19 scroll1")]
    #[test_case::test_case(195, 19, usize::MAX; "w195 h19 scroll2")]
    #[test_case::test_case(195, 61, 0; "w195 h61 scroll0")]
    #[test_case::test_case(195, 61, 7; "w195 h61 scroll1")]
    #[test_case::test_case(195, 61, usize::MAX; "w195 h61 scroll2")]
    #[test_case::test_case(196, 0, 0; "w196 h0 scroll0")]
    #[test_case::test_case(196, 0, 7; "w196 h0 scroll1")]
    #[test_case::test_case(196, 0, usize::MAX; "w196 h0 scroll2")]
    #[test_case::test_case(196, 1, 0; "w196 h1 scroll0")]
    #[test_case::test_case(196, 1, 7; "w196 h1 scroll1")]
    #[test_case::test_case(196, 1, usize::MAX; "w196 h1 scroll2")]
    #[test_case::test_case(196, 19, 0; "w196 h19 scroll0")]
    #[test_case::test_case(196, 19, 7; "w196 h19 scroll1")]
    #[test_case::test_case(196, 19, usize::MAX; "w196 h19 scroll2")]
    #[test_case::test_case(196, 61, 0; "w196 h61 scroll0")]
    #[test_case::test_case(196, 61, 7; "w196 h61 scroll1")]
    #[test_case::test_case(196, 61, usize::MAX; "w196 h61 scroll2")]
    #[test_case::test_case(197, 0, 0; "w197 h0 scroll0")]
    #[test_case::test_case(197, 0, 7; "w197 h0 scroll1")]
    #[test_case::test_case(197, 0, usize::MAX; "w197 h0 scroll2")]
    #[test_case::test_case(197, 1, 0; "w197 h1 scroll0")]
    #[test_case::test_case(197, 1, 7; "w197 h1 scroll1")]
    #[test_case::test_case(197, 1, usize::MAX; "w197 h1 scroll2")]
    #[test_case::test_case(197, 19, 0; "w197 h19 scroll0")]
    #[test_case::test_case(197, 19, 7; "w197 h19 scroll1")]
    #[test_case::test_case(197, 19, usize::MAX; "w197 h19 scroll2")]
    #[test_case::test_case(197, 61, 0; "w197 h61 scroll0")]
    #[test_case::test_case(197, 61, 7; "w197 h61 scroll1")]
    #[test_case::test_case(197, 61, usize::MAX; "w197 h61 scroll2")]
    #[test_case::test_case(235, 0, 0; "w235 h0 scroll0")]
    #[test_case::test_case(235, 0, 7; "w235 h0 scroll1")]
    #[test_case::test_case(235, 0, usize::MAX; "w235 h0 scroll2")]
    #[test_case::test_case(235, 1, 0; "w235 h1 scroll0")]
    #[test_case::test_case(235, 1, 7; "w235 h1 scroll1")]
    #[test_case::test_case(235, 1, usize::MAX; "w235 h1 scroll2")]
    #[test_case::test_case(235, 19, 0; "w235 h19 scroll0")]
    #[test_case::test_case(235, 19, 7; "w235 h19 scroll1")]
    #[test_case::test_case(235, 19, usize::MAX; "w235 h19 scroll2")]
    #[test_case::test_case(235, 61, 0; "w235 h61 scroll0")]
    #[test_case::test_case(235, 61, 7; "w235 h61 scroll1")]
    #[test_case::test_case(235, 61, usize::MAX; "w235 h61 scroll2")]
    fn help_bodies_fit_small_rectangles(width: usize, height: usize, scroll: usize) {
        let (body, _) = help_body(width, height, scroll, C, Quit::Keys);
        assert!(
            body.len() <= height,
            "{width}x{height}: body has {} rows",
            body.len()
        );
        for line in &body {
            assert!(
                line_width(line, C) <= width,
                "{width}x{height}: body writes beyond rectangle: {:?}",
                plain(line, C)
            );
        }
    }

    #[test]
    fn the_title_rule_keeps_whole_words_and_a_dash_track() {
        let text = |title, width| plain(&title_rule(title, width, C), C);
        assert_eq!(text("NIGHT AND DAY", 13), "NIGHT AND DAY");
        assert_eq!(text("NIGHT AND DAY", 16), "NIGHT AND DAY ──");
        assert_eq!(text("NIGHT AND DAY", 9), "NIGHT... ");
        assert_eq!(text("NIGHT AND DAY", 0), "");
        assert_eq!(text("COVERAGE", 6), " ─────");
        for width in 0..30 {
            assert!(line_width(&title_rule("NIGHT AND DAY", width, C), C) <= width);
        }
    }

    #[test_case("uncovered", 20, 6, ""; "omit a word that cannot fit")]
    #[test_case("bound by rate", 30, 10, "bound..."; "shorten at a word")]
    #[test_case("bound by rate", 60, 15, "bound by rate"; "keep a fitting label")]
    fn help_labels_keep_whole_words(
        label: &'static str,
        width: usize,
        label_width: usize,
        want: &str,
    ) {
        let section = Section {
            title: "WORDS",
            items: &[(label, "evidence")],
        };
        let lines = section_lines(&section, width, false, C);
        let row = plain(&lines[2], C);
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

    #[test]
    fn help_examples_keep_whole_values() {
        let examples: [(&[&str], bool); 3] = [
            (&["►", "04", "-12", "LOADED", "-31"], true),
            (&["09", "-8"], false),
            (&["11", "?", "-50"], true),
        ];
        for width in 1..=100 {
            let lines = reading(width, C);
            let mut at = 2;
            for (i, (tokens, has_edge)) in examples.iter().enumerate() {
                let row = plain(&lines[at], C);
                let fields: Vec<&str> = row.split_whitespace().collect();
                for field in &fields {
                    assert!(
                        field.trim_matches(['▄', '·']).is_empty() || tokens.contains(field),
                        "width {width} example {i}: text or value cut to {field:?}: {row:?}"
                    );
                }
                assert!(
                    line_width(&lines[at], C) <= width,
                    "width {width} example {i}: row exceeds width: {row:?}"
                );
                if width == 100 {
                    let edge_ok = !has_edge || fields.last() == tokens.last();
                    assert!(
                        edge_ok
                            && fields.contains(&tokens[tokens.len() - 1 - usize::from(*has_edge)]),
                        "wide example {i} lost its state or night edge: {row:?}"
                    );
                }
                while !plain(&lines[at], C).is_empty() {
                    at += 1;
                }
                at += 1;
            }
        }
    }

    #[test_case(13)]
    #[test_case(20)]
    fn the_help_view_examples_keep_whole_values(width: u16) {
        let sc = screen(View::Help, width, 60, C, false);
        let (buf, _) = render_missing(&sc);
        for line in rows_of(&buf) {
            for field in line.split_whitespace() {
                let digits = field
                    .strip_prefix('-')
                    .filter(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()));
                assert!(
                    digits.is_none()
                        || ["-8", "-12", "-14", "-26", "-30", "-31", "-50"].contains(&field),
                    "width {width}: help example cut a value to {field:?}: {line:?}"
                );
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
            usize::from(layout.body.height),
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
        assert!(rows[usize::from(layout.body.y)].contains("READING THE SCREEN"));
        assert!(rows[usize::from(layout.footer.y)].contains("exit dashboard"));
    }

    #[test]
    fn help_labels_the_quit_key_exit_dashboard_and_the_tuning_boot_drops_it() {
        let words = |quit| {
            plain_rows(&help_body(115, 1000, 0, C, quit).0)
                .join("\n")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let standalone = words(Quit::Keys);
        assert!(standalone.contains("q exit dashboard; tuning itself keeps running"));
        assert!(!standalone.contains("closes this view"), "{standalone}");
        let boot = words(Quit::Ignored);
        for gone in ["exit dashboard", "closes this view", "itself keeps running"] {
            assert!(
                !boot.contains(gone),
                "tuning-boot help says {gone:?}: {boot}"
            );
        }
        assert!(
            boot.contains("last observed"),
            "the rest of the top line stays"
        );
    }

    #[test_case::test_case(77, 19; "77x19")]
    #[test_case::test_case(77, 1000; "77x1000")]
    #[test_case::test_case(115, 19; "115x19")]
    #[test_case::test_case(115, 1000; "115x1000")]
    #[test_case::test_case(235, 19; "235x19")]
    #[test_case::test_case(235, 1000; "235x1000")]
    fn tuning_boot_help_fits_its_body(width: usize, height: usize) {
        let (body, _) = help_body(width, height, usize::MAX, C, Quit::Ignored);
        assert!(body.len() <= height);
    }

    #[test_case::test_case(80, 24, |extent| extent; "80x24 at end")]
    #[test_case::test_case(80, 24, |extent| extent + 1; "80x24 past end")]
    #[test_case::test_case(80, 24, |_| 100_000; "80x24 far")]
    #[test_case::test_case(160, 45, |extent| extent; "160x45 at end")]
    #[test_case::test_case(160, 45, |extent| extent + 1; "160x45 past end")]
    #[test_case::test_case(160, 45, |_| 100_000; "160x45 far")]
    #[test_case::test_case(240, 67, |extent| extent; "240x67 at end")]
    #[test_case::test_case(240, 67, |extent| extent + 1; "240x67 past end")]
    #[test_case::test_case(240, 67, |_| 100_000; "240x67 far")]
    #[test_case::test_case(240, 30, |extent| extent; "240x30 at end")]
    #[test_case::test_case(240, 30, |extent| extent + 1; "240x30 past end")]
    #[test_case::test_case(240, 30, |_| 100_000; "240x30 far")]
    fn a_scroll_at_or_past_the_extent_follows_the_end(
        width: u16,
        height: u16,
        sample: fn(usize) -> usize,
    ) {
        let mut sc = screen(View::Help, width, height, C, true);
        sc.scroll = usize::MAX;
        let (end, drawn) = render_missing(&sc);
        let scroll = sample(drawn.scroll);
        sc.scroll = scroll;
        assert_eq!(
            render_missing(&sc).0,
            end,
            "{width}x{height} scroll {scroll}"
        );
        sc.scroll = drawn.scroll.saturating_sub(1);
        if drawn.scroll > 0 {
            assert_ne!(
                render_missing(&sc).0,
                end,
                "{width}x{height}: one line above the end is not the end"
            );
        }
    }

    #[test_case::test_case(240, 67, Colours::Full, 0, true; "240x67 Full scroll 0 keys True")]
    #[test_case::test_case(240, 67, Colours::Full, 0, false; "240x67 Full scroll 0 keys False")]
    #[test_case::test_case(240, 67, Colours::Full, 3, true; "240x67 Full scroll 1 keys True")]
    #[test_case::test_case(240, 67, Colours::Full, 3, false; "240x67 Full scroll 1 keys False")]
    #[test_case::test_case(240, 67, Colours::Full, usize::MAX, true; "240x67 Full scroll 2 keys True")]
    #[test_case::test_case(240, 67, Colours::Full, usize::MAX, false; "240x67 Full scroll 2 keys False")]
    #[test_case::test_case(240, 67, Colours::Full, 100_000, true; "240x67 Full scroll 3 keys True")]
    #[test_case::test_case(240, 67, Colours::Full, 100_000, false; "240x67 Full scroll 3 keys False")]
    #[test_case::test_case(240, 67, Colours::Console, 0, true; "240x67 Console scroll 0 keys True")]
    #[test_case::test_case(240, 67, Colours::Console, 0, false; "240x67 Console scroll 0 keys False")]
    #[test_case::test_case(240, 67, Colours::Console, 3, true; "240x67 Console scroll 1 keys True")]
    #[test_case::test_case(240, 67, Colours::Console, 3, false; "240x67 Console scroll 1 keys False")]
    #[test_case::test_case(240, 67, Colours::Console, usize::MAX, true; "240x67 Console scroll 2 keys True")]
    #[test_case::test_case(240, 67, Colours::Console, usize::MAX, false; "240x67 Console scroll 2 keys False")]
    #[test_case::test_case(240, 67, Colours::Console, 100_000, true; "240x67 Console scroll 3 keys True")]
    #[test_case::test_case(240, 67, Colours::Console, 100_000, false; "240x67 Console scroll 3 keys False")]
    #[test_case::test_case(240, 67, Colours::None, 0, true; "240x67 None scroll 0 keys True")]
    #[test_case::test_case(240, 67, Colours::None, 0, false; "240x67 None scroll 0 keys False")]
    #[test_case::test_case(240, 67, Colours::None, 3, true; "240x67 None scroll 1 keys True")]
    #[test_case::test_case(240, 67, Colours::None, 3, false; "240x67 None scroll 1 keys False")]
    #[test_case::test_case(240, 67, Colours::None, usize::MAX, true; "240x67 None scroll 2 keys True")]
    #[test_case::test_case(240, 67, Colours::None, usize::MAX, false; "240x67 None scroll 2 keys False")]
    #[test_case::test_case(240, 67, Colours::None, 100_000, true; "240x67 None scroll 3 keys True")]
    #[test_case::test_case(240, 67, Colours::None, 100_000, false; "240x67 None scroll 3 keys False")]
    #[test_case::test_case(160, 45, Colours::Full, 0, true; "160x45 Full scroll 0 keys True")]
    #[test_case::test_case(160, 45, Colours::Full, 0, false; "160x45 Full scroll 0 keys False")]
    #[test_case::test_case(160, 45, Colours::Full, 3, true; "160x45 Full scroll 1 keys True")]
    #[test_case::test_case(160, 45, Colours::Full, 3, false; "160x45 Full scroll 1 keys False")]
    #[test_case::test_case(160, 45, Colours::Full, usize::MAX, true; "160x45 Full scroll 2 keys True")]
    #[test_case::test_case(160, 45, Colours::Full, usize::MAX, false; "160x45 Full scroll 2 keys False")]
    #[test_case::test_case(160, 45, Colours::Full, 100_000, true; "160x45 Full scroll 3 keys True")]
    #[test_case::test_case(160, 45, Colours::Full, 100_000, false; "160x45 Full scroll 3 keys False")]
    #[test_case::test_case(160, 45, Colours::Console, 0, true; "160x45 Console scroll 0 keys True")]
    #[test_case::test_case(160, 45, Colours::Console, 0, false; "160x45 Console scroll 0 keys False")]
    #[test_case::test_case(160, 45, Colours::Console, 3, true; "160x45 Console scroll 1 keys True")]
    #[test_case::test_case(160, 45, Colours::Console, 3, false; "160x45 Console scroll 1 keys False")]
    #[test_case::test_case(160, 45, Colours::Console, usize::MAX, true; "160x45 Console scroll 2 keys True")]
    #[test_case::test_case(160, 45, Colours::Console, usize::MAX, false; "160x45 Console scroll 2 keys False")]
    #[test_case::test_case(160, 45, Colours::Console, 100_000, true; "160x45 Console scroll 3 keys True")]
    #[test_case::test_case(160, 45, Colours::Console, 100_000, false; "160x45 Console scroll 3 keys False")]
    #[test_case::test_case(160, 45, Colours::None, 0, true; "160x45 None scroll 0 keys True")]
    #[test_case::test_case(160, 45, Colours::None, 0, false; "160x45 None scroll 0 keys False")]
    #[test_case::test_case(160, 45, Colours::None, 3, true; "160x45 None scroll 1 keys True")]
    #[test_case::test_case(160, 45, Colours::None, 3, false; "160x45 None scroll 1 keys False")]
    #[test_case::test_case(160, 45, Colours::None, usize::MAX, true; "160x45 None scroll 2 keys True")]
    #[test_case::test_case(160, 45, Colours::None, usize::MAX, false; "160x45 None scroll 2 keys False")]
    #[test_case::test_case(160, 45, Colours::None, 100_000, true; "160x45 None scroll 3 keys True")]
    #[test_case::test_case(160, 45, Colours::None, 100_000, false; "160x45 None scroll 3 keys False")]
    #[test_case::test_case(120, 33, Colours::Full, 0, true; "120x33 Full scroll 0 keys True")]
    #[test_case::test_case(120, 33, Colours::Full, 0, false; "120x33 Full scroll 0 keys False")]
    #[test_case::test_case(120, 33, Colours::Full, 3, true; "120x33 Full scroll 1 keys True")]
    #[test_case::test_case(120, 33, Colours::Full, 3, false; "120x33 Full scroll 1 keys False")]
    #[test_case::test_case(120, 33, Colours::Full, usize::MAX, true; "120x33 Full scroll 2 keys True")]
    #[test_case::test_case(120, 33, Colours::Full, usize::MAX, false; "120x33 Full scroll 2 keys False")]
    #[test_case::test_case(120, 33, Colours::Full, 100_000, true; "120x33 Full scroll 3 keys True")]
    #[test_case::test_case(120, 33, Colours::Full, 100_000, false; "120x33 Full scroll 3 keys False")]
    #[test_case::test_case(120, 33, Colours::Console, 0, true; "120x33 Console scroll 0 keys True")]
    #[test_case::test_case(120, 33, Colours::Console, 0, false; "120x33 Console scroll 0 keys False")]
    #[test_case::test_case(120, 33, Colours::Console, 3, true; "120x33 Console scroll 1 keys True")]
    #[test_case::test_case(120, 33, Colours::Console, 3, false; "120x33 Console scroll 1 keys False")]
    #[test_case::test_case(120, 33, Colours::Console, usize::MAX, true; "120x33 Console scroll 2 keys True")]
    #[test_case::test_case(120, 33, Colours::Console, usize::MAX, false; "120x33 Console scroll 2 keys False")]
    #[test_case::test_case(120, 33, Colours::Console, 100_000, true; "120x33 Console scroll 3 keys True")]
    #[test_case::test_case(120, 33, Colours::Console, 100_000, false; "120x33 Console scroll 3 keys False")]
    #[test_case::test_case(120, 33, Colours::None, 0, true; "120x33 None scroll 0 keys True")]
    #[test_case::test_case(120, 33, Colours::None, 0, false; "120x33 None scroll 0 keys False")]
    #[test_case::test_case(120, 33, Colours::None, 3, true; "120x33 None scroll 1 keys True")]
    #[test_case::test_case(120, 33, Colours::None, 3, false; "120x33 None scroll 1 keys False")]
    #[test_case::test_case(120, 33, Colours::None, usize::MAX, true; "120x33 None scroll 2 keys True")]
    #[test_case::test_case(120, 33, Colours::None, usize::MAX, false; "120x33 None scroll 2 keys False")]
    #[test_case::test_case(120, 33, Colours::None, 100_000, true; "120x33 None scroll 3 keys True")]
    #[test_case::test_case(120, 33, Colours::None, 100_000, false; "120x33 None scroll 3 keys False")]
    #[test_case::test_case(80, 24, Colours::Full, 0, true; "80x24 Full scroll 0 keys True")]
    #[test_case::test_case(80, 24, Colours::Full, 0, false; "80x24 Full scroll 0 keys False")]
    #[test_case::test_case(80, 24, Colours::Full, 3, true; "80x24 Full scroll 1 keys True")]
    #[test_case::test_case(80, 24, Colours::Full, 3, false; "80x24 Full scroll 1 keys False")]
    #[test_case::test_case(80, 24, Colours::Full, usize::MAX, true; "80x24 Full scroll 2 keys True")]
    #[test_case::test_case(80, 24, Colours::Full, usize::MAX, false; "80x24 Full scroll 2 keys False")]
    #[test_case::test_case(80, 24, Colours::Full, 100_000, true; "80x24 Full scroll 3 keys True")]
    #[test_case::test_case(80, 24, Colours::Full, 100_000, false; "80x24 Full scroll 3 keys False")]
    #[test_case::test_case(80, 24, Colours::Console, 0, true; "80x24 Console scroll 0 keys True")]
    #[test_case::test_case(80, 24, Colours::Console, 0, false; "80x24 Console scroll 0 keys False")]
    #[test_case::test_case(80, 24, Colours::Console, 3, true; "80x24 Console scroll 1 keys True")]
    #[test_case::test_case(80, 24, Colours::Console, 3, false; "80x24 Console scroll 1 keys False")]
    #[test_case::test_case(80, 24, Colours::Console, usize::MAX, true; "80x24 Console scroll 2 keys True")]
    #[test_case::test_case(80, 24, Colours::Console, usize::MAX, false; "80x24 Console scroll 2 keys False")]
    #[test_case::test_case(80, 24, Colours::Console, 100_000, true; "80x24 Console scroll 3 keys True")]
    #[test_case::test_case(80, 24, Colours::Console, 100_000, false; "80x24 Console scroll 3 keys False")]
    #[test_case::test_case(80, 24, Colours::None, 0, true; "80x24 None scroll 0 keys True")]
    #[test_case::test_case(80, 24, Colours::None, 0, false; "80x24 None scroll 0 keys False")]
    #[test_case::test_case(80, 24, Colours::None, 3, true; "80x24 None scroll 1 keys True")]
    #[test_case::test_case(80, 24, Colours::None, 3, false; "80x24 None scroll 1 keys False")]
    #[test_case::test_case(80, 24, Colours::None, usize::MAX, true; "80x24 None scroll 2 keys True")]
    #[test_case::test_case(80, 24, Colours::None, usize::MAX, false; "80x24 None scroll 2 keys False")]
    #[test_case::test_case(80, 24, Colours::None, 100_000, true; "80x24 None scroll 3 keys True")]
    #[test_case::test_case(80, 24, Colours::None, 100_000, false; "80x24 None scroll 3 keys False")]
    fn help_stays_in_bounds_and_draws_only_console_glyphs_at_every_size(
        width: u16,
        height: u16,
        colours: Colours,
        scroll: usize,
        keys: bool,
    ) {
        let mut sc = screen(View::Help, width, height, colours, keys);
        sc.scroll = scroll;
        let (buf, _) = render_missing(&sc);
        assert_frame_bounds(&buf, &sc);
        assert_glyphs_drawable(
            &buf,
            &format!("help {width}x{height} {colours:?} scroll {scroll} keys {keys}"),
        );
    }

    #[test]
    fn help_stays_in_bounds_at_every_small_width() {
        for width in 0..=80 {
            for height in [0, 1, 2, 3, 4, 5, 8, 24] {
                let sc = screen(View::Help, width, height, Colours::Full, true);
                let (buf, _) = render_missing(&sc);
                assert_frame_bounds(&buf, &sc);
            }
        }
    }

    #[test_case(Colours::Rich, "▆▆▆", "red ▆", "amber ▆"; "rich")]
    #[test_case(Colours::Full, "▄▄▄", "red ▄", "amber ▄"; "console")]
    fn the_legend_names_the_profiles_own_gauge_glyphs(
        colours: Colours,
        applied: &str,
        edge: &str,
        censored: &str,
    ) {
        let (help, _) = help_body(115, 1000, 0, colours, Quit::Keys);
        let rows = plain_rows_in(&help, colours);
        let row_of = |label: &str| {
            rows.iter()
                .find(|row| row.chars().skip(2).collect::<String>().starts_with(label))
                .unwrap_or_else(|| panic!("no legend row {label:?}: {rows:#?}"))
                .clone()
        };
        assert!(row_of(applied).contains("offset applied now"));
        assert!(row_of(edge).contains("the night edge U of its CCD"));
        assert!(row_of(censored).contains("a censored edge"));
        let text = rows.join("\n");
        let rich = colours == Colours::Rich;
        assert!(!text.contains('▬'));
        assert!(!text.contains('▀'));
        assert_eq!(text.contains('▄'), !rich, "the console's bar glyph");
    }

    #[test_case(Colours::Console; "console")]
    #[test_case(Colours::Rich; "rich")]
    fn examples_draw_the_edge_marks_at_their_counts(colours: Colours) {
        let marks = super::marks(colours);
        let lines = reading(100, colours);
        let gauge_of = |core: &str| {
            let row = lines
                .iter()
                .map(|line| plain(line, colours))
                .find(|text| {
                    text.trim_start()
                        .trim_start_matches('►')
                        .trim_start()
                        .starts_with(core)
                })
                .unwrap_or_else(|| panic!("no example row for core {core}"));
            let at = row.find([marks.applied, '·']).expect("a gauge");
            row[at..].chars().take(50).collect::<Vec<_>>()
        };
        let loaded = gauge_of("04");
        assert!(loaded[..12].iter().all(|&c| c == marks.applied));
        assert!(loaded[12..26].iter().all(|&c| c == '·'));
        assert_eq!(loaded[30], marks.edge, "-31 is the 31st cell");
        let censored = gauge_of("11");
        assert!(censored[..30].iter().all(|&c| c == '·'));
        assert_eq!(censored[49], marks.edge, "-50 is the 50th cell");
    }
}
