//! The journal view.

use jiff::tz::TimeZone;
use ratatui::text::{Line, Span};

use super::super::colours::Paint;
use super::super::fit::{Clause, Form, cut_words, display_width, fit_clauses, rule};
use super::super::help::title_rule;
use super::super::scene::wall_second;
use super::super::scroll::scroll_body_with_bar;
use super::table::wall_date;
use crate::history::{LOG_LIMIT, LogEntry};
use crate::screen::{Colours, Log};
use crate::words::{noun, plural};

/// The cells of the tag column in the journal view.
const TAG_CELLS: usize = 20;
/// The cells of a log row before its message: the time, a gap, the tag column and a gap.
const LOG_PREFIX: usize = 8 + 2 + TAG_CELLS + 2;

/// The journal view's body in `width` cells and `height` rows scrolled `scroll` lines down, with its scrollbar;
/// and how far it can scroll. A scroll at or past that extent shows the end.
pub(super) fn body(
    log: &Log<'_>,
    width: usize,
    height: usize,
    scroll: usize,
    zone: &TimeZone,
    colours: Colours,
) -> (Vec<Line<'static>>, usize) {
    let inner = width.saturating_sub(2).max(1);
    let mut lines = vec![rule_line(log, inner, colours), Line::default()];
    if log.entries.is_empty() {
        lines.push(Line::styled(
            cut_words("No journal entries yet.", inner),
            Paint::Grey.style(colours),
        ));
    }
    lines.extend(entry_rows(log.entries, inner, zone, colours));
    scroll_body_with_bar(&lines, width, height, scroll, colours)
}

/// The trial a `trial progress` entry reports a passed self-test for, from its message `trial N self-test 4K
/// passed` or `trial N core 02 self-test 4K passed`.
fn self_test_trial(entry: &LogEntry) -> Option<&str> {
    if entry.tag != "trial progress" {
        return None;
    }
    let mut words = entry.msg.split_whitespace().peekable();
    let (Some("trial"), Some(trial)) = (words.next(), words.next()) else {
        return None;
    };
    if words.next_if_eq(&"core").is_some() {
        words.next()?;
    }
    let rest: Vec<&str> = words.collect();
    (rest.first() == Some(&"self-test") && rest.last() == Some(&"passed")).then_some(trial)
}

/// The journal's rows: a separator at each local date, and each run of one trial's passed self-tests folded
/// into one row that counts them. Everything else is one row per event.
fn entry_rows(
    entries: &[LogEntry],
    width: usize,
    zone: &TimeZone,
    colours: Colours,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut date = String::new();
    let mut i = 0;
    while i < entries.len() {
        let entry = &entries[i];
        let today = wall_date(zone, entry.at);
        if today != date {
            out.push(rule(
                width,
                &Line::from(Paint::Grey.span(colours, today.clone())),
                &Line::default(),
                Paint::Track.style(colours),
            ));
            date = today;
        }
        let Some(trial) = self_test_trial(entry) else {
            out.push(row(entry, width, zone, colours));
            i += 1;
            continue;
        };
        let run = entries[i..]
            .iter()
            .take_while(|next| {
                self_test_trial(next) == Some(trial) && wall_date(zone, next.at) == date
            })
            .count();
        let folded = LogEntry {
            seq: entry.seq,
            at: entry.at,
            tag: entry.tag.clone(),
            msg: format!("trial {trial}: {}", folded_count(run)),
        };
        out.push(row(&folded, width, zone, colours));
        i += run;
    }
    out
}

fn folded_count(n: usize) -> String {
    format!("{n} self-test {} passed", noun(n, "check"))
}

/// The rule that heads the journal view: how many events the journal holds that the view lists, and how many
/// of them it shows. SMU and preflight events are never listed, and the rule says so while it has room.
fn rule_line(log: &Log<'_>, width: usize, colours: Colours) -> Line<'static> {
    let events = |n: usize| plural(n, "event");
    let notes = if log.total > LOG_LIMIT {
        vec![
            format!(
                "last {LOG_LIMIT} of {} · SMU and preflight left out",
                events(log.total)
            ),
            format!("last {LOG_LIMIT} of {}", events(log.total)),
        ]
    } else {
        vec![
            format!("{} · SMU and preflight left out", events(log.total)),
            events(log.total),
        ]
    };
    for note in notes {
        let room = width.saturating_sub(display_width(&note) + 1);
        if "JOURNAL".len() < room {
            let mut line = title_rule("JOURNAL", room, colours);
            line.spans.push(Span::raw(" "));
            line.spans.push(Paint::Grey.span(colours, note));
            return line;
        }
    }
    title_rule("JOURNAL", width, colours)
}

/// One journal line in `width` cells: the time, the 20-cell tag column and the message when a whole word of the
/// message fits beside them, else the time and the tag, else the time, else nothing.
fn row(entry: &LogEntry, width: usize, zone: &TimeZone, colours: Colours) -> Line<'static> {
    let stamp = Paint::Grey.span(colours, wall_second(zone, entry.at));
    let tag = cut_words(&entry.tag, TAG_CELLS);
    let text = cut_words(&entry.msg, width.saturating_sub(LOG_PREFIX));
    if !text.is_empty() {
        let pad = " ".repeat(TAG_CELLS.saturating_sub(display_width(&tag)));
        return Line::from(vec![
            stamp,
            Span::raw("  "),
            Paint::Text.span(colours, format!("{tag}{pad}")),
            Span::raw("  "),
            Paint::Text.span(colours, text),
        ]);
    }
    fit_clauses(
        width,
        &[
            Clause::whole("", Line::from(stamp)),
            Clause::new(
                "  ",
                vec![
                    Form::new(Line::styled(tag, Paint::Text.style(colours)), 0),
                    Form::empty(),
                ],
            ),
        ],
    )
}

/// The footer clause that counts the events arrived since the journal view held its list; none when none
/// arrived.
pub(super) fn arrivals(log: &Log<'_>, colours: Colours) -> Vec<Clause> {
    if log.arrived == 0 {
        return Vec::new();
    }
    let count = format!("{} {}", log.arrived, noun(log.arrived, "new event"));
    let amber = |text: String| Line::styled(text, Paint::Hunt.style(colours));
    vec![Clause::new(
        "   ",
        vec![
            Form::new(amber(format!("{count} · End: latest")), 2),
            Form::new(amber(count), 2),
            Form::empty(),
        ],
    )]
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use jiff::Timestamp;
    use jiff::tz::{TimeZone, offset};
    use ratatui::text::Line;

    use super::super::super::colours::Paint;
    use super::super::super::fit::{CUT_MARKER, cut_words, fit_clauses, line_width, plain};
    use super::super::super::footer::key_hints;
    use super::super::fixtures::at;
    use super::{arrivals, body, rule_line};
    use crate::history::{LOG_LIMIT, LogEntry};
    use crate::screen::{Colours, Log, Quit, View};
    use crate::tests::kit::seq;

    const START: &str = "session dashboard started by a togi build from before version stamps (schema 0, ruleset 0, fixes 0, 3 cores)";

    fn entry(n: u64, at: Timestamp, tag: &'static str, msg: &str) -> LogEntry {
        LogEntry {
            seq: seq(n),
            at,
            tag: Cow::Borrowed(tag),
            msg: msg.to_owned(),
        }
    }

    fn log(entries: &[LogEntry]) -> Log<'_> {
        Log {
            entries,
            total: entries.len(),
            arrived: 0,
        }
    }

    fn rows(log: &Log<'_>, width: usize, height: usize, scroll: usize) -> (Vec<String>, usize) {
        let (page, extent) = body(log, width, height, scroll, &TimeZone::UTC, Colours::None);
        (page.iter().map(plain).collect(), extent)
    }

    fn text_cells(row: &str) -> String {
        row.chars()
            .skip(2)
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn a_row_keeps_whole_values_at_every_width() {
        let entries = [entry(1, at(1000), "start", START)];
        let log = log(&entries);
        let stamp = "00:16:40";
        let pad = " ".repeat(20 - "start".len());
        for (text, want) in [
            (5, String::new()),
            (8, stamp.to_owned()),
            (11, stamp.to_owned()),
            (14, stamp.to_owned()),
            (15, format!("{stamp}  start")),
            (26, format!("{stamp}  start")),
            (31, format!("{stamp}  start")),
            (32, format!("{stamp}  start")),
            (41, format!("{stamp}  start")),
            (42, format!("{stamp}  start{pad}  session...")),
            (52, format!("{stamp}  start{pad}  session dashboard...")),
            (58, format!("{stamp}  start{pad}  session dashboard...")),
            (155, format!("{stamp}  start{pad}  {START}")),
        ] {
            let (page, _) = rows(&log, text + 2, 20, 0);
            assert_eq!(text_cells(&page[3]), want, "{text} text cells");
        }
        let words: Vec<&str> = START.split_whitespace().collect();
        for width in 6..=160 {
            let (page, _) = rows(&log, width, 20, 0);
            let row = text_cells(&page[3]);
            assert!(row.chars().count() < width.max(3), "width {width}: {row:?}");
            let fields: Vec<&str> = row.split_whitespace().collect();
            if let Some(first) = fields.first() {
                assert_eq!(*first, stamp, "width {width} cuts its time: {row:?}");
            }
            if let Some(second) = fields.get(1) {
                assert_eq!(*second, "start", "width {width} cuts its tag: {row:?}");
            }
            let message = &fields[fields.len().min(2)..];
            assert!(message.len() <= words.len(), "width {width}: {row:?}");
            for (i, field) in message.iter().enumerate() {
                let (mut field, mut want) = (*field, words[i]);
                if i + 1 == message.len() && field.ends_with(CUT_MARKER) {
                    field = field.trim_end_matches(CUT_MARKER);
                    field = field.trim_end_matches([',', ';', ':']);
                    want = want.trim_end_matches([',', ';', ':']);
                } else {
                    assert!(
                        i + 1 < message.len() || message.len() == words.len(),
                        "width {width}: message ends early without {CUT_MARKER}: {row:?}"
                    );
                }
                assert_eq!(field, want, "width {width}: {row:?}");
            }
        }
    }

    #[test]
    fn an_empty_journal_says_so_in_whole_words() {
        let log = log(&[]);
        for (text, want) in [
            (3, ""),
            (5, "No..."),
            (15, "No journal..."),
            (22, "No journal entries..."),
            (23, "No journal entries yet."),
            (115, "No journal entries yet."),
        ] {
            let (page, _) = rows(&log, text + 2, 20, 0);
            assert_eq!(text_cells(&page[2]), want, "{text} text cells");
        }
    }

    #[test]
    fn rows_align_their_tags_in_local_time() {
        let at = Timestamp::from_second(1_767_311_998).expect("a timestamp");
        let entries = [
            entry(1, at, "start", "first message"),
            entry(
                2,
                at + jiff::SignedDuration::from_secs(1),
                "crash.detected",
                "second message",
            ),
        ];
        let log = log(&entries);
        let east = TimeZone::fixed(offset(3));
        let (page, _) = body(&log, 120, 10, 0, &east, Colours::None);
        let page: Vec<String> = page.iter().map(plain).collect();
        assert_eq!(
            text_cells(&page[3]),
            format!("02:59:58  start{}  first message", " ".repeat(15))
        );
        assert_eq!(
            text_cells(&page[4]),
            format!("02:59:59  crash.detected{}  second message", " ".repeat(6))
        );
        let (utc, _) = rows(&log, 120, 10, 0);
        assert!(utc[3].contains("23:59:58"), "{}", utc[3]);
    }

    #[test]
    fn untrusted_tags_and_messages_are_escaped_at_output() {
        let entries = [entry(1, at(1000), "trial.end", "bad \u{1b}[31mnews\u{7}")];
        let (page, _) = rows(&log(&entries), 80, 10, 0);
        assert!(page[3].contains("bad \\x1b[31mnews\\x07"), "{}", page[3]);
        assert!(!page[3].contains('\u{1b}') && !page[3].contains('\u{7}'));
    }

    #[test]
    fn a_separator_names_each_local_date_before_its_events() {
        let late = Timestamp::from_second(1_767_311_998).expect("a timestamp");
        let entries = [
            entry(1, late, "start", "first"),
            entry(
                2,
                late + jiff::SignedDuration::from_secs(1),
                "start",
                "same day in UTC",
            ),
            entry(
                3,
                late + jiff::SignedDuration::from_secs(90_000),
                "start",
                "next day",
            ),
        ];
        let log = log(&entries);
        let (utc, _) = rows(&log, 80, 20, 0);
        assert!(utc[2].contains("2026-01-01"), "{utc:?}");
        assert!(utc[5].contains("2026-01-03"), "{utc:?}");
        let east = TimeZone::fixed(offset(3));
        let (page, _) = body(&log, 80, 20, 0, &east, Colours::None);
        let page: Vec<String> = page.iter().map(plain).collect();
        assert!(page[2].contains("2026-01-02"), "{page:?}");
        assert!(page[3].contains("02:59:58"), "{page:?}");
        assert!(page[4].contains("02:59:59"), "{page:?}");
        assert!(page[5].contains("2026-01-03"), "{page:?}");
    }

    #[test]
    fn one_trials_passed_self_tests_fold_into_a_count() {
        let progress = |n: u64, msg: &str| {
            entry(
                n,
                at(1000 + i64::try_from(n).unwrap()),
                "trial progress",
                msg,
            )
        };
        let entries = [
            progress(1, "trial t1 core 02 self-test 4K passed"),
            progress(2, "trial t1 core 03 self-test 8K passed"),
            progress(3, "trial t1 self-test 16K passed"),
            progress(4, "trial t2 core 00 self-test 4K passed"),
            progress(5, "trial t2 core 01 self-test 4K failed"),
        ];
        let (page, _) = rows(&log(&entries), 100, 20, 0);
        let texts: Vec<String> = page.iter().map(|row| text_cells(row)).collect();
        assert!(
            texts[3].ends_with("trial t1: 3 self-test checks passed"),
            "{texts:?}"
        );
        assert!(
            texts[4].ends_with("trial t2: 1 self-test check passed"),
            "{texts:?}"
        );
        assert!(
            texts[5].ends_with("trial t2 core 01 self-test 4K failed"),
            "{texts:?}"
        );
        assert!(
            texts.iter().all(|row| !row.contains("4K passed")),
            "{texts:?}"
        );
    }

    fn thirty(extra: bool) -> Vec<LogEntry> {
        let start = Timestamp::from_second(1_767_268_800).expect("a timestamp");
        let mut entries: Vec<LogEntry> = (0..30)
            .map(|i| {
                entry(
                    i + 1,
                    start + jiff::SignedDuration::from_secs(i64::try_from(i).expect("small")),
                    "trial.end",
                    &format!("entry {i:02}"),
                )
            })
            .collect();
        if extra {
            entries.push(entry(
                31,
                start + jiff::SignedDuration::from_secs(30),
                "crash.detected",
                "new crash",
            ));
        }
        entries
    }

    #[test]
    fn the_view_follows_new_entries_only_at_the_end() {
        let before = thirty(false);
        let (top, top_extent) = rows(&log(&before), 77, 10, 0);
        let (end, end_extent) = rows(&log(&before), 77, 10, usize::MAX);
        assert_eq!(top_extent, end_extent);
        let last = end.last().expect("a row");
        for want in ["12:00:29", "trial.end", "entry 29"] {
            assert!(last.contains(want), "last entry loses {want:?}: {last}");
        }
        let after = thirty(true);
        let (next_top, _) = rows(&log(&after), 77, 10, 0);
        let (next_end, next_extent) = rows(&log(&after), 77, 10, usize::MAX);
        for (row, (want, got)) in top.iter().zip(&next_top).enumerate().skip(1) {
            assert_eq!(text_cells(want), text_cells(got), "pinned page row {row}");
        }
        assert_eq!(next_extent, end_extent + 1);
        let last = next_end.last().expect("a row");
        for want in ["12:00:30", "crash.detected", "new crash"] {
            assert!(last.contains(want), "the end loses {want:?}: {last}");
        }
    }

    #[test]
    fn a_scroll_at_or_past_the_extent_shows_the_end() {
        let entries = thirty(true);
        let (end, extent) = rows(&log(&entries), 77, 10, usize::MAX);
        for scroll in [extent, extent + 1, extent + 100] {
            assert_eq!(
                rows(&log(&entries), 77, 10, scroll).0,
                end,
                "scroll {scroll}"
            );
        }
        let (above, _) = rows(&log(&entries), 77, 10, extent - 1);
        assert_ne!(above, end);
    }

    #[test]
    fn arrivals_never_move_a_row() {
        let entries = thirty(false);
        let held = log(&entries);
        let (page, extent) = rows(&held, 77, 10, 3);
        for arrived in [1, 8, 123] {
            let later = Log { arrived, ..held };
            assert_eq!(rows(&later, 77, 10, 3), (page.clone(), extent));
        }
    }

    #[test]
    fn the_rule_says_how_many_events_the_journal_holds_that_the_view_lists() {
        let rule = |total: usize, width: usize| {
            let log = Log {
                entries: &[],
                total,
                arrived: 0,
            };
            plain(&rule_line(&log, width, Colours::None))
        };
        assert!(rule(12, 80).starts_with("JOURNAL"));
        assert!(rule(12, 80).ends_with("12 events · SMU and preflight left out"));
        assert!(rule(1, 80).ends_with("1 event · SMU and preflight left out"));
        assert!(rule(12, 30).ends_with("12 events"));
        assert!(rule(LOG_LIMIT + 77, 90).ends_with(&format!(
            "last {LOG_LIMIT} of {} events · SMU and preflight left out",
            LOG_LIMIT + 77
        )));
        assert!(
            rule(LOG_LIMIT + 77, 40)
                .ends_with(&format!("last {LOG_LIMIT} of {} events", LOG_LIMIT + 77))
        );
        assert_eq!(rule(12, 12), "JOURNAL ────");
        assert_eq!(rule(12, 3), " ──");
        for width in 1..120 {
            assert!(
                line_width(&rule_line(
                    &Log {
                        entries: &[],
                        total: 500,
                        arrived: 0
                    },
                    width,
                    Colours::None
                )) <= width
            );
        }
    }

    #[test]
    fn log_bodies_fit_small_rectangles() {
        let entries = [entry(
            1,
            at(0),
            "trial.intent",
            &"long journal line ".repeat(20),
        )];
        let log = log(&entries);
        for width in [0, 1, 2, 10, 77, 155, 194, 195, 196, 197, 235] {
            for height in [0, 1, 19, 61] {
                for scroll in [0, 7, usize::MAX] {
                    let (page, _) =
                        body(&log, width, height, scroll, &TimeZone::UTC, Colours::None);
                    assert!(
                        page.len() <= height,
                        "{width}x{height}: {} rows",
                        page.len()
                    );
                    for line in &page {
                        assert!(
                            line_width(line) <= width,
                            "{width}x{height}: {:?}",
                            plain(line)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_footer_counts_what_arrived_and_names_the_way_back() {
        let none = Log {
            entries: &[],
            total: 0,
            arrived: 0,
        };
        assert!(arrivals(&none, Colours::None).is_empty());
        for (arrived, count) in [(1, "1 new event"), (8, "8 new events")] {
            let held = Log { arrived, ..none };
            let clause = arrivals(&held, Colours::Full);
            assert_eq!(clause.len(), 1);
            let line = |width| plain(&fit_clauses(width, &clause));
            assert_eq!(line(80), format!("{count} · End: latest"));
            assert_eq!(line(count.len() + 4), count);
            assert_eq!(line(count.len()), count);
            assert_eq!(line(3), "");
            assert_eq!(cut_words(&line(80), 80), line(80));
        }
    }

    #[test]
    fn the_arrival_count_keeps_the_keys_in_the_footer() {
        let keys = ["←/→", "Esc", "?", "q"];
        for arrived in [1, 8] {
            let held = Log {
                entries: &[],
                total: 0,
                arrived,
            };
            let count = if arrived == 1 {
                "1 new event".to_owned()
            } else {
                format!("{arrived} new events")
            };
            let clause = arrivals(&held, Colours::Full);
            let footer = |width| key_hints(View::Log, width, &clause, Colours::Full, Quit::Keys);
            let wide = plain(&footer(120));
            assert!(wide.contains(&format!("{count} · End: latest")), "{wide}");
            let reference = footer(120);
            for width in [24, 30, 40, 50] {
                let line = footer(width);
                let text = plain(&line);
                let fields: Vec<&str> = text.split_whitespace().collect();
                for key in keys {
                    assert!(
                        fields.contains(&key),
                        "width {width} lost {key:?}: {text:?}"
                    );
                    let styled = |line: &Line<'_>| {
                        line.spans
                            .iter()
                            .find(|span| span.content.trim() == key)
                            .map(|span| span.style)
                    };
                    assert_eq!(
                        styled(&line),
                        styled(&reference),
                        "width {width} key {key:?}"
                    );
                }
                assert!(
                    !text.contains(CUT_MARKER),
                    "width {width} cut text: {text:?}"
                );
                assert!(
                    !text.contains("new") || text.contains(&count),
                    "width {width}: {text:?}"
                );
                if width == 50 {
                    assert!(text.contains(&count), "{text:?}");
                }
                assert!(line_width(&line) <= width);
            }
            for width in [4, 5, 6, 10] {
                let text = plain(&footer(width));
                let fields: Vec<&str> = text.split_whitespace().collect();
                assert!(fields.contains(&"q"), "width {width}: {text:?}");
                let words: Vec<&str> = count.split_whitespace().collect();
                for field in fields {
                    assert!(
                        keys.contains(&field) || words.contains(&field),
                        "width {width} cut a key or the notice: {text:?}"
                    );
                }
            }
            let amber = reference
                .spans
                .iter()
                .find(|span| span.content.contains(&count))
                .expect("the count span");
            assert_eq!(amber.style, Paint::Hunt.style(Colours::Full));
        }
    }
}
