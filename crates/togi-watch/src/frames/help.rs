//! The help view: static explanations of the screen, in three columns when wide and one scrolling column
//! otherwise.
//!
//! Labels and examples keep whole words and values at every width: a help
//! example's gauge is clipped by cells first, and its failure point, state
//! word or offset are left out whole, never cut. The view shows a scrollbar
//! whenever its contents need scrolling, and scrolling to the end reaches the
//! last text row, not trailing blank rows.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::colours::Paint;
use super::fit::{cut_words, display_width, line_width, truncate_styled, wrap_styled};
use super::scroll::scroll_body_with_bar;
use crate::screen::Colours;

struct Section {
    title: &'static str,
    items: &'static [(&'static str, &'static str)],
}

const SCREEN: Section = Section {
    title: "READING THE SCREEN",
    items: &[
        ("►", "the core carries load in this trial"),
        (
            "▄▄▄",
            "offset applied now, or saved after a stop restored the hardware; white when this trial judges the core",
        ),
        (
            "grey ▄▄▄",
            "the same depth, grey when the core is not judged",
        ),
        (
            "···",
            "depth it reached but does not use now: given back, parked, waiting",
        ),
        ("░░░", "depth a combination blocks now"),
        (
            "▬",
            "its failure point: this core failed there on its own account",
        ),
        ("▀", "a group trial failed with this core at that offset"),
        ("cyan", "running now"),
        ("amber", "a hunt, and what it paused"),
        ("red", "failures, past and present"),
        ("green", "passed, done"),
        ("magenta", "combinations"),
        ("grey", "idle, parked, still to come"),
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
        ("q", "closes this view only; tuning keeps running"),
    ],
};

const TUNING: Section = Section {
    title: "WHAT TOGI DOES",
    items: &[
        (
            "SOLO LIMITS",
            "The cores whose solo limit is found, and the core being searched or confirmed.",
        ),
        (
            "PHASE 1",
            "The first full cycle, run on the profile the solo limits give.",
        ),
        (
            "HUNT",
            "An amber detour: parked trials that find the core or combination behind a failure. The hunt panel shows its groups and member probes.",
        ),
        (
            "PHASE 2",
            "Deepening: its rounds and its confirmation cycle. A round lists its candidates and their gaps.",
        ),
        (
            "BIOS PROFILE",
            "The profile to enter in BIOS, one row per CCD in slot order. A * marks an offset not yet confirmed.",
        ),
        (
            "CYCLE",
            "Checking: every core at its offset together, through the steps of a cycle. The stage shows the cycle's number and step; the cycle panel lists its steps and R7 parts.",
        ),
        (
            "CLEAN CYCLES",
            "The count of clean cycles for the current profile.",
        ),
        (
            "R6",
            "During idle trials this screen holds still, clock included, so it cannot wake the cores.",
        ),
        (
            "NO ETA",
            "A hunt can start at any time, so togi shows only what is scheduled and the time left in the current trial.",
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
            "failure point",
            "the shallowest offset where a core failed on its own account",
        ),
        (
            "combination",
            "offsets at which a group of cores failed together",
        ),
        ("at its limit", "a core that cannot go deeper"),
        ("has room", "a core that can still go deeper"),
        ("suspect", "a core a hunt keeps at its failing offset"),
        (
            "parked",
            "a core a hunt holds shallower than its failing offset",
        ),
        ("partial part", "an R7 part with the top requesters idle"),
        (
            "top requester",
            "the loaded core with the highest voltage request, setting the shared core voltage; marked top on the CCD tables",
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
    section: &Section,
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

/// A help example keeps the text of its row whole: its heads run from the full core, offset and state to
/// nothing, each with its failure point or without, and the first that fits wins. Only the gauge is clipped by
/// cells.
fn example_row(
    width: usize,
    heads: &[String],
    gauge: &Line<'static>,
    failure: &str,
    colours: Colours,
) -> Line<'static> {
    for head in heads {
        for with_failure in [true, false] {
            let head_width = display_width(head);
            let used = head_width
                + if with_failure {
                    2 + display_width(failure)
                } else {
                    0
                };
            if used > width {
                continue;
            }
            let mut spans = Vec::new();
            if !head.is_empty() {
                spans.push(Paint::White.span(colours, head.clone()));
            }
            spans.extend(gauge.spans.iter().cloned());
            let mut line = truncate_styled(&Line::from(spans), width - used + head_width);
            if with_failure {
                line.spans.push(Span::raw("  "));
                line.spans
                    .push(Paint::Bad.span(colours, failure.to_owned()));
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
        .map(|&(_, glyph, count)| display_width(glyph) * count)
        .sum();
    spans.push(Span::raw(" ".repeat(50usize.saturating_sub(used))));
    Line::from(spans)
}

fn reading(width: usize, colours: Colours) -> Vec<Line<'static>> {
    struct Example {
        core: u32,
        offset: i32,
        failure: i32,
        state: &'static str,
        note: &'static str,
        gauge: Line<'static>,
    }
    let examples = [
        Example {
            core: 4,
            offset: -28,
            failure: -36,
            state: "AT LIMIT",
            note: "at its limit: it cannot go deeper",
            gauge: gauge(
                colours,
                &[
                    (Paint::White, "▄", 28),
                    (Paint::Combination, "░", 7),
                    (Paint::Bad, "▬", 1),
                ],
            ),
        },
        Example {
            core: 0,
            offset: -26,
            failure: -32,
            state: "HAS ROOM",
            note: "has room: backed off after a hunt, may deepen",
            gauge: gauge(
                colours,
                &[
                    (Paint::White, "▄", 26),
                    (Paint::Grey, "·", 5),
                    (Paint::Bad, "▬", 1),
                ],
            ),
        },
        Example {
            core: 9,
            offset: 0,
            failure: -48,
            state: "PARKED",
            note: "parked at 0 by a hunt; returns to -47",
            gauge: gauge(colours, &[(Paint::Grey, "·", 47), (Paint::Bad, "▬", 1)]),
        },
        Example {
            core: 0,
            offset: -26,
            failure: -32,
            state: "PROBE",
            note: "probe: the group failed with core 00 at -30, -29, -27",
            gauge: gauge(
                colours,
                &[(Paint::White, "▄", 26), (Paint::Bad, "▀ ▀▀ ▬", 1)],
            ),
        },
    ];
    let mut out = vec![title_rule(SCREEN.title, width, colours), blank()];
    for example in examples {
        let marker = if example.state == "PARKED" {
            "  "
        } else {
            "► "
        };
        let Example {
            core,
            offset,
            failure,
            state,
            ..
        } = example;
        let prefix = format!("{marker}{core:02}  {offset:3}  {state:<8} ");
        let heads = [
            prefix.clone(),
            format!("{marker}{core:02}  {offset:3} "),
            format!("{marker}{core:02} "),
            String::new(),
        ];
        out.push(example_row(
            width,
            &heads,
            &example.gauge,
            &format!("{failure:3}"),
            colours,
        ));
        let indent = display_width(&prefix).min(width.saturating_sub(20));
        for line in wrap_styled(
            example.note,
            width.saturating_sub(indent).max(1),
            Paint::Grey.style(colours),
        ) {
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            spans.extend(line.spans);
            out.push(Line::from(spans));
        }
        out.push(blank());
    }
    out.extend(
        section_lines(&SCREEN, width, false, colours)
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
) -> (Vec<Line<'static>>, usize) {
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
                section_lines(&TOP, left, false, colours),
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
        let mut lines = reading(body_width, colours);
        for section in [&TOP, &TUNING, &WORDS, &LOAD] {
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
    use crate::screen::{Colours, Drawn, Hold, View};

    const C: Colours = Colours::Full;

    fn plain_rows(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines.iter().map(plain).collect()
    }

    fn chars_from(text: &str, from: usize) -> String {
        text.chars().skip(from).collect()
    }

    fn first_char(text: &str) -> char {
        text.chars().next().unwrap_or(' ')
    }

    #[test]
    fn wide_help_uses_three_columns() {
        let (body, scroll) = help_body(235, 61, 0, C);
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
        for title in ["THE TOP LINE", "KINDS OF LOAD", "NO ETA", "CLEAN CYCLES"] {
            assert!(text.contains(title), "wide help lost {title:?}");
        }
        for (row, line) in body.iter().enumerate() {
            assert!(line_width(line) <= 235, "row {row} exceeds help rectangle");
        }
        assert!(
            !rows[rows.len() - 1].trim().is_empty(),
            "wide help ends on a blank content row"
        );
        for section in [&TOP, &TUNING, &LOAD] {
            let lines = section_lines(section, 70, section.title == TUNING.title, C);
            assert!(
                !plain(&lines[lines.len() - 1]).trim().is_empty(),
                "wide column's final section {:?} ends on blank padding",
                section.title
            );
        }
        for width in [195, 235] {
            let (top, scroll) = help_body(width, 19, 0, C);
            let (end, end_scroll) = help_body(width, 19, usize::MAX, C);
            let (clamped, _) = help_body(width, 19, scroll + 100, C);
            assert!(
                scroll != 0 && end_scroll == scroll,
                "short wide help must scroll: top={scroll} end={end_scroll}"
            );
            assert_eq!(plain_rows(&end), plain_rows(&clamped), "scroll beyond end");
            for title in ["READING THE SCREEN", "WHAT TOGI DOES", "WORDS"] {
                assert!(
                    plain(&top[0]).contains(title),
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
    }

    #[test]
    fn help_glosses_no_record_only_trial() {
        let (help, _) = help_body(115, 1000, 0, C);
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
    fn help_links_the_tuner_spec_and_states_no_rule() {
        let (help, _) = help_body(115, 1000, 0, C);
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
        for width in [115, 235] {
            let (body, scroll) = help_body(width, 1000, 0, C);
            assert!(
                scroll == 0 && !body.is_empty(),
                "{width} columns: full help was not rendered"
            );
            let mut column = 0;
            if width >= THREE_COLUMNS {
                let first = plain(&body[0]);
                let prefix = first
                    .split_once("WORDS")
                    .unwrap_or_else(|| panic!("wide help lost the words column: {first}"))
                    .0;
                column = prefix.chars().count();
            }
            let lines: Vec<String> = body
                .iter()
                .enumerate()
                .map(|(row, line)| {
                    assert!(
                        line_width(line) <= width,
                        "{width} columns: full help row {row} exceeds its rectangle"
                    );
                    chars_from(&plain(line), column)
                })
                .collect();
            let words = lines
                .join("\n")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            for explanation in [
                "SEARCH finding its solo limit with light and heavy-vector trials alone",
                "CONFIRM checking its candidate solo limit with repeated light and heavy-vector trials alone",
                "FOUND solo limit checked; waiting at 0 WAITING",
                "WAITING at 0, waiting for its first search turn",
                "MEMBER kept with its hunt group at its failing offset; another member may be probed",
            ] {
                assert!(
                    words.contains(explanation),
                    "{width} columns: help lost state explanation {explanation:?}"
                );
            }
            assert!(
                !plain(&body[body.len() - 1]).trim().is_empty(),
                "{width} columns: full help ends on a blank content row"
            );
        }
    }

    #[test]
    fn narrow_help_scrolls_one_column() {
        let (top, scroll) = help_body(77, 19, 0, C);
        let (end, end_scroll) = help_body(77, 19, usize::MAX, C);
        let (clamped, _) = help_body(77, 19, scroll + 100, C);
        assert!(
            scroll != 0 && end_scroll == scroll,
            "narrow help must scroll: top={scroll} end={end_scroll}"
        );
        assert_eq!(plain_rows(&end), plain_rows(&clamped), "scroll beyond end");
        assert!(
            plain(&top[0]).contains("READING THE SCREEN"),
            "help does not open at the first section: {:?}",
            plain(&top[0])
        );
        let text = plain_rows(&end).join("\n");
        assert!(
            text.contains("KINDS OF LOAD") && text.contains("R7 all-core"),
            "help end loses the last section: {text}"
        );
        assert!(
            !plain(&top[0]).contains("WHAT TOGI DOES"),
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
    }

    #[test]
    fn help_bodies_fit_small_rectangles() {
        for width in [0, 1, 2, 10, 77, 155, 194, 195, 196, 197, 235] {
            for height in [0, 1, 19, 61] {
                for scroll in [0, 7, usize::MAX] {
                    let (body, _) = help_body(width, height, scroll, C);
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

    #[test]
    fn help_labels_keep_whole_words() {
        for (items, width, label_width, want) in [
            (&[("requester", "evidence")], 20, 6, ""),
            (&[("top requester", "evidence")], 30, 10, "top..."),
            (&[("top requester", "evidence")], 60, 15, "top requester"),
        ] {
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
    }

    #[test]
    fn help_examples_keep_whole_values() {
        let examples: [&[&str]; 4] = [
            &["►", "04", "-28", "AT", "LIMIT", "-36"],
            &["►", "00", "-26", "HAS", "ROOM", "-32"],
            &["09", "0", "PARKED", "-48"],
            &["►", "00", "-26", "PROBE", "-32"],
        ];
        for width in 1..=100 {
            let lines = reading(width, C);
            let mut at = 2;
            for (i, tokens) in examples.iter().enumerate() {
                let row = plain(&lines[at]);
                let fields: Vec<&str> = row.split_whitespace().collect();
                for field in &fields {
                    assert!(
                        field.trim_matches(['▄', '░', '·', '▬', '▀']).is_empty()
                            || tokens.contains(field),
                        "width {width} example {i}: text or value cut to {field:?}: {row:?}"
                    );
                }
                for pair in [("AT", "LIMIT"), ("HAS", "ROOM")] {
                    assert_eq!(
                        fields.contains(&pair.0),
                        fields.contains(&pair.1),
                        "width {width} example {i}: state word cut: {row:?}"
                    );
                }
                assert!(
                    line_width(&lines[at]) <= width,
                    "width {width} example {i}: row exceeds width: {row:?}"
                );
                if width == 100 {
                    assert!(
                        fields.last() == tokens.last()
                            && fields.contains(&tokens[tokens.len() - 2]),
                        "wide example {i} lost its state or failure point: {row:?}"
                    );
                }
                while !plain(&lines[at]).is_empty() {
                    at += 1;
                }
                at += 1;
            }
        }
    }

    #[test]
    fn the_help_view_examples_keep_whole_values() {
        for width in [13, 20] {
            let sc = screen(View::Help, width, 60, C, false);
            let (buf, _) = render_missing(&sc);
            for line in rows_of(&buf) {
                let fields: Vec<&str> = line.split_whitespace().collect();
                assert!(
                    !fields.contains(&"-2"),
                    "width {width}: help example cut a value: {line:?}"
                );
                assert_eq!(
                    fields.contains(&"AT"),
                    fields.contains(&"LIMIT"),
                    "width {width}: help example cut a state word: {line:?}"
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
        assert!(rows[usize::from(layout.footer.y)].contains("close view"));
    }

    #[test]
    fn a_scroll_at_or_past_the_extent_follows_the_end() {
        for (width, height) in [(80, 24), (160, 45), (240, 67), (240, 30)] {
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
    }

    #[test]
    fn help_stays_in_bounds_and_draws_only_console_glyphs_at_every_size() {
        for (width, height) in [(240, 67), (160, 45), (120, 33), (80, 24)] {
            for colours in [Colours::Full, Colours::Console, Colours::None] {
                for scroll in [0, 3, usize::MAX, 100_000] {
                    for keys in [true, false] {
                        let mut sc = screen(View::Help, width, height, colours, keys);
                        sc.scroll = scroll;
                        let (buf, _) = render_missing(&sc);
                        assert_frame_bounds(&buf, &sc);
                        assert_glyphs_drawable(&buf, &format!("help {width}x{height}"));
                    }
                }
            }
        }
        for width in 0..=80 {
            for height in [0, 1, 2, 3, 4, 5, 8, 24] {
                let sc = screen(View::Help, width, height, Colours::Full, true);
                let (buf, _) = render_missing(&sc);
                assert_frame_bounds(&buf, &sc);
            }
        }
    }
}
