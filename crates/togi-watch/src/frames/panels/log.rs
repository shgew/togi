//! The journal view.

use jiff::tz::TimeZone;
use ratatui::text::{Line, Span};

use super::super::colours::Paint;
use super::super::fit::{Clause, Form, cut_words, display_width, fit_clauses};
use super::super::help::title_rule;
use super::super::scene::wall_second;
use super::super::scroll::scroll_body_with_bar;
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
            cut_words("No journal entries yet.", inner, colours),
            Paint::Grey.style(colours),
        ));
    }
    lines.extend(
        log.entries
            .iter()
            .map(|entry| row(entry, inner, zone, colours)),
    );
    scroll_body_with_bar(&lines, width, height, scroll, colours)
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
        let room = width.saturating_sub(display_width(&note, colours) + 1);
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
    let tag = cut_words(&entry.tag, TAG_CELLS, colours);
    let text = cut_words(&entry.msg, width.saturating_sub(LOG_PREFIX), colours);
    if !text.is_empty() {
        let pad = " ".repeat(TAG_CELLS.saturating_sub(display_width(&tag, colours)));
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
        colours,
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
    use super::super::super::fit::{cut_marker, cut_words, fit_clauses, line_width, plain};
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
        (
            page.iter().map(|line| plain(line, Colours::None)).collect(),
            extent,
        )
    }

    fn text_cells(row: &str) -> String {
        row.chars()
            .skip(2)
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test_case::test_case(5, ""; "five")]
    #[test_case::test_case(8, "00:16:40"; "eight")]
    #[test_case::test_case(11, "00:16:40"; "eleven")]
    #[test_case::test_case(14, "00:16:40"; "fourteen")]
    #[test_case::test_case(15, "00:16:40  start"; "fifteen")]
    #[test_case::test_case(26, "00:16:40  start"; "twenty six")]
    #[test_case::test_case(31, "00:16:40  start"; "thirty one")]
    #[test_case::test_case(32, "00:16:40  start"; "thirty two")]
    #[test_case::test_case(41, "00:16:40  start"; "forty one")]
    #[test_case::test_case(42, "00:16:40  start                 session..."; "forty two")]
    #[test_case::test_case(52, "00:16:40  start                 session dashboard..."; "fifty two")]
    #[test_case::test_case(58, "00:16:40  start                 session dashboard..."; "fifty eight")]
    #[test_case::test_case(155, concat!("00:16:40  start                 ", "session dashboard started by a togi build from before version stamps (schema 0, ruleset 0, fixes 0, 3 cores)"); "full")]
    fn a_row_keeps_whole_values_at_selected_widths(text: usize, want: &str) {
        let entries = [entry(1, at(1000), "start", START)];
        let (page, _) = rows(&log(&entries), text + 2, 20, 0);
        assert_eq!(text_cells(&page[2]), want, "{text} text cells");
    }

    #[test]
    fn a_row_keeps_whole_values_at_every_width() {
        let entries = [entry(1, at(1000), "start", START)];
        let log = log(&entries);
        let stamp = "00:16:40";
        let words: Vec<&str> = START.split_whitespace().collect();
        for width in 6..=160 {
            let (page, _) = rows(&log, width, 20, 0);
            let row = text_cells(&page[2]);
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
                if i + 1 == message.len() && field.ends_with(cut_marker(Colours::None)) {
                    field = field.trim_end_matches(cut_marker(Colours::None));
                    field = field.trim_end_matches([',', ';', ':']);
                    want = want.trim_end_matches([',', ';', ':']);
                } else {
                    assert!(
                        i + 1 < message.len() || message.len() == words.len(),
                        "width {width}: message ends early without {}: {row:?}",
                        cut_marker(Colours::None)
                    );
                }
                assert_eq!(field, want, "width {width}: {row:?}");
            }
        }
    }

    #[test_case::test_case(3, ""; "three")]
    #[test_case::test_case(5, "No..."; "five")]
    #[test_case::test_case(15, "No journal..."; "fifteen")]
    #[test_case::test_case(22, "No journal entries..."; "twenty two")]
    #[test_case::test_case(23, "No journal entries yet."; "twenty three")]
    #[test_case::test_case(115, "No journal entries yet."; "wide")]
    fn an_empty_journal_says_so_in_whole_words(text: usize, want: &str) {
        let log = log(&[]);
        let (page, _) = rows(&log, text + 2, 20, 0);
        assert_eq!(text_cells(&page[2]), want, "{text} text cells");
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
        let page: Vec<String> = page.iter().map(|line| plain(line, Colours::None)).collect();
        assert_eq!(
            text_cells(&page[2]),
            format!("02:59:58  start{}  first message", " ".repeat(15))
        );
        assert_eq!(
            text_cells(&page[3]),
            format!("02:59:59  crash.detected{}  second message", " ".repeat(6))
        );
        let (utc, _) = rows(&log, 120, 10, 0);
        assert!(utc[2].contains("23:59:58"), "{}", utc[2]);
    }

    #[test]
    fn untrusted_tags_and_messages_are_escaped_at_output() {
        let entries = [entry(1, at(1000), "trial.end", "bad \u{1b}[31mnews\u{7}")];
        let (page, _) = rows(&log(&entries), 80, 10, 0);
        assert!(page[2].contains("bad \\x1b[31mnews\\x07"), "{}", page[2]);
        assert!(!page[2].contains('\u{1b}') && !page[2].contains('\u{7}'));
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

    #[test_case::test_case(0; "at extent")]
    #[test_case::test_case(1; "one past extent")]
    #[test_case::test_case(100; "far past extent")]
    fn a_scroll_at_or_past_the_extent_shows_the_end(extra: usize) {
        let entries = thirty(true);
        let (end, extent) = rows(&log(&entries), 77, 10, usize::MAX);
        let scroll = extent + extra;
        assert_eq!(
            rows(&log(&entries), 77, 10, scroll).0,
            end,
            "scroll {scroll}"
        );
        let (above, _) = rows(&log(&entries), 77, 10, extent - 1);
        assert_ne!(above, end);
    }

    #[test_case::test_case(1; "one")]
    #[test_case::test_case(8; "eight")]
    #[test_case::test_case(123; "many")]
    fn arrivals_never_move_a_row(arrived: usize) {
        let entries = thirty(false);
        let held = log(&entries);
        let (page, extent) = rows(&held, 77, 10, 3);
        let later = Log { arrived, ..held };
        assert_eq!(
            rows(&later, 77, 10, 3),
            (page.clone(), extent),
            "arrived {arrived}"
        );
    }

    #[test]
    fn the_rule_says_how_many_events_the_journal_holds_that_the_view_lists() {
        let rule = |total: usize, width: usize| {
            let log = Log {
                entries: &[],
                total,
                arrived: 0,
            };
            plain(&rule_line(&log, width, Colours::None), Colours::None)
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
                line_width(
                    &rule_line(
                        &Log {
                            entries: &[],
                            total: 500,
                            arrived: 0
                        },
                        width,
                        Colours::None
                    ),
                    Colours::None
                ) <= width,
                "width {width}"
            );
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
    fn log_bodies_fit_small_rectangles(width: usize, height: usize, scroll: usize) {
        let entries = [entry(
            1,
            at(0),
            "trial.intent",
            &"long journal line ".repeat(20),
        )];
        let log = log(&entries);
        let (page, _) = body(&log, width, height, scroll, &TimeZone::UTC, Colours::None);
        assert!(
            page.len() <= height,
            "{width}x{height}: {} rows",
            page.len()
        );
        for line in &page {
            assert!(
                line_width(line, Colours::None) <= width,
                "{width}x{height}: {:?}",
                plain(line, Colours::None)
            );
        }
    }

    #[test_case::test_case(1, "1 new event"; "one")]
    #[test_case::test_case(8, "8 new events"; "eight")]
    fn the_footer_counts_what_arrived_and_names_the_way_back(arrived: usize, count: &str) {
        let none = Log {
            entries: &[],
            total: 0,
            arrived: 0,
        };
        assert!(arrivals(&none, Colours::None).is_empty());
        let held = Log { arrived, ..none };
        let clause = arrivals(&held, Colours::Full);
        assert_eq!(clause.len(), 1);
        let line = |width| plain(&fit_clauses(width, &clause, Colours::None), Colours::None);
        assert_eq!(line(80), format!("{count} · End: latest"));
        assert_eq!(line(count.len() + 4), count);
        assert_eq!(line(count.len()), count);
        assert_eq!(line(3), "");
        assert_eq!(cut_words(&line(80), 80, Colours::None), line(80));
    }

    #[test_case::test_case(1, 20; "arrived 1 width 20")]
    #[test_case::test_case(1, 30; "arrived 1 width 30")]
    #[test_case::test_case(1, 40; "arrived 1 width 40")]
    #[test_case::test_case(1, 50; "arrived 1 width 50")]
    #[test_case::test_case(1, 4; "arrived 1 width 4")]
    #[test_case::test_case(1, 5; "arrived 1 width 5")]
    #[test_case::test_case(1, 6; "arrived 1 width 6")]
    #[test_case::test_case(1, 10; "arrived 1 width 10")]
    #[test_case::test_case(8, 20; "arrived 8 width 20")]
    #[test_case::test_case(8, 30; "arrived 8 width 30")]
    #[test_case::test_case(8, 40; "arrived 8 width 40")]
    #[test_case::test_case(8, 50; "arrived 8 width 50")]
    #[test_case::test_case(8, 4; "arrived 8 width 4")]
    #[test_case::test_case(8, 5; "arrived 8 width 5")]
    #[test_case::test_case(8, 6; "arrived 8 width 6")]
    #[test_case::test_case(8, 10; "arrived 8 width 10")]
    fn the_arrival_count_keeps_the_keys_in_the_footer(arrived: usize, width: usize) {
        let keys = ["?", "l", "esc", "q"];
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
        let wide = plain(&footer(120), Colours::None);
        assert!(wide.contains(&format!("{count} · End: latest")), "{wide}");
        let reference = footer(120);
        if width >= 20 {
            let line = footer(width);
            let text = plain(&line, Colours::None);
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
                !text.contains(cut_marker(Colours::None)),
                "width {width} cut text: {text:?}"
            );
            assert!(
                !text.contains("new") || text.contains(&count),
                "width {width}: {text:?}"
            );
            if width == 50 {
                assert!(text.contains(&count), "{text:?}");
            }
            assert!(line_width(&line, Colours::None) <= width);
        } else {
            let text = plain(&footer(width), Colours::None);
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
