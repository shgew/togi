//! What happened: the history panel.

use ratatui::text::{Line, Span};

use super::super::colours::Paint;
use super::super::fit::{Clause, Form, cut_styled, fit_clauses, line_width, rule};
use super::super::scene::Scene;
use super::table::cell;
use crate::history::{Entry, Failed, LOG_LIMIT, What};

/// The cells of a row before its sentence: the time, a gap, the tag column and a gap.
const PREFIX: usize = 16;

/// What happened, newest first, in `height` rows of `width` cells. Its last row counts every entry it leaves
/// out: those that do not fit and those older than the entries the model keeps.
pub(super) fn history(scene: &Scene<'_>, width: usize, height: usize) -> Vec<Line<'static>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let entries = scene.s.history;
    let room = height.saturating_sub(2);
    let shown = if scene.s.history_dropped > 0 || entries.len() > room {
        entries.len().min(room.saturating_sub(1))
    } else {
        entries.len().min(room)
    };
    let hidden = scene.s.history_dropped + entries.len() - shown;
    let header_rows = if hidden > 0 {
        (height - 1).min(2)
    } else {
        height.min(2)
    };
    let mut out = Vec::with_capacity(header_rows + shown + usize::from(hidden > 0));
    if header_rows > 0 {
        let right = if width < 100 {
            format!("l: last {LOG_LIMIT}")
        } else {
            format!("newest first · l: last {LOG_LIMIT} events")
        };
        out.push(rule(
            width,
            &cell(scene, Paint::Grey, "WHAT HAPPENED"),
            &cell(scene, Paint::Grey, right),
            scene.style(Paint::Track),
            scene.colours,
        ));
    }
    if header_rows > 1 {
        out.push(Line::default());
    }
    for entry in &entries[..shown] {
        out.push(row(scene, entry, width));
    }
    if hidden > 0 {
        out.push(cell(scene, Paint::Grey, format!("+{hidden} more")));
    }
    #[cfg(test)]
    crate::corpus::lines("panel.history", "", &out);
    out
}

fn row(scene: &Scene<'_>, entry: &Entry, width: usize) -> Line<'static> {
    let room = width.saturating_sub(PREFIX);
    let mut spans = vec![
        scene.paint(Paint::Grey, scene.wall_minute(entry.at)),
        Span::raw("  "),
        scene.paint(Paint::from(entry.tone), format!("{:<8}", entry.tag.word())),
        Span::raw(" "),
    ];
    let sentence = if let What::Failed(failed) = &entry.what {
        verdict_sentence(scene, failed, room)
    } else {
        let (before, alarm, after) = entry.sentence();
        cut_styled(
            &Line::from(vec![
                scene.paint(Paint::Text, before),
                scene.paint(Paint::Bad, alarm),
                scene.paint(Paint::Text, after),
            ]),
            room,
            scene.colours,
        )
    };
    spans.extend(sentence.spans);
    Line::from(spans)
}

/// The sentence of a failed trial in `room` cells. What the failure was and whom it names are reserved first;
/// the timing gives way before anything else, and the subject is cut to what is left, so a long subject never
/// takes the attribution's cells.
fn verdict_sentence(scene: &Scene<'_>, failed: &Failed, room: usize) -> Line<'static> {
    let verdict = &failed.verdict;
    let sep = || Line::from(scene.paint(Paint::Text, " · "));
    let signal_paint = if verdict.alarm() {
        Paint::Bad
    } else {
        Paint::Text
    };
    let signal = Line::from(scene.paint(signal_paint, verdict.signal_words()));
    let named = verdict
        .named
        .map(|named| Line::from(scene.paint(Paint::Text, named.words())));
    let reserved: usize = line_width(&signal, scene.colours)
        + named.as_ref().map_or(0, |named| {
            line_width(named, scene.colours) + line_width(&sep(), scene.colours)
        });
    let subject_room = room.saturating_sub(reserved + line_width(&sep(), scene.colours));
    let subject_text = Line::from(scene.paint(Paint::Text, failed.subject.clone()));
    let subject_cut = cut_styled(&subject_text, subject_room, scene.colours);
    let mut subject = vec![Form::new(subject_text, 1)];
    subject.push(if line_width(&subject_cut, scene.colours) > 0 {
        Form::new(subject_cut, 0)
    } else {
        Form::empty()
    });
    let mut clauses = vec![
        Clause::new(Line::default(), subject),
        Clause::whole(sep(), signal),
    ];
    if let Some(when) = &verdict.when {
        clauses.push(Clause::new(
            sep(),
            vec![
                Form::new(Line::from(scene.paint(Paint::Text, when.clone())), 2),
                Form::empty(),
            ],
        ));
    }
    if let Some(named) = named {
        clauses.push(Clause::whole(sep(), named));
    }
    fit_clauses(room, &clauses, scene.colours)
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;
    use ratatui::text::Line;
    use test_case::test_case;

    use super::super::super::colours::Paint;
    use super::super::super::fit::{cut_words, line_width, plain};
    use super::super::fixtures::{at, scene, texts};
    use super::history;
    use crate::frames::frame_tests::journal;
    use crate::history::{Entry, HISTORY_LIMIT};
    use crate::model::Model;
    use crate::model::project;
    use crate::screen::Colours;
    use crate::tests::kit::warning;
    use crate::words::{Tag, Tone};

    fn entries(n: usize) -> Vec<Entry> {
        (0..n)
            .map(|i| {
                Entry::line(
                    at(1000 + i64::try_from(i).expect("a small count")),
                    Tag::Note,
                    format!("warning {:03}", n - 1 - i),
                    Tone::Plain,
                )
            })
            .collect()
    }

    fn panel(kept: &[Entry], dropped: usize, width: usize, height: usize) -> Vec<Line<'static>> {
        let model = Model::new();
        let mut s = model.snapshot();
        s.history = kept;
        s.history_dropped = dropped;
        let zone = TimeZone::UTC;
        history(&scene(&s, &zone), width, height)
    }

    fn assert_budget(
        name: &str,
        rows: &[String],
        width: usize,
        height: usize,
        want: usize,
        footer: &str,
    ) {
        assert!(
            rows.len() <= height,
            "{name}: {} rows for {height}: {rows:?}",
            rows.len()
        );
        assert_eq!(rows.len(), want, "{name}: {rows:?}");
        for (i, row) in rows.iter().enumerate() {
            assert!(
                row.chars().count() <= width,
                "{name}: row {i} exceeds {width} cells: {row:?}"
            );
            if row.starts_with('+') {
                assert!(
                    i == rows.len() - 1 && row == footer,
                    "{name}: row {i}: {row:?}"
                );
            }
        }
        if !footer.is_empty() && !rows.is_empty() {
            assert_eq!(rows.last().map(String::as_str), Some(footer));
        }
    }

    #[test_case(0, 0, 0, ""; "no rows")]
    #[test_case(1, 1, 0, "+701 more"; "one row")]
    #[test_case(2, 2, 0, "+701 more"; "two rows")]
    #[test_case(3, 3, 0, "+701 more"; "three rows")]
    #[test_case(10, 10, 7, "+694 more"; "ten rows")]
    #[test_case(202, 202, 199, "+502 more"; "one short of every entry")]
    #[test_case(203, 203, 200, "+501 more"; "every entry")]
    #[test_case(222, 203, 200, "+501 more"; "more room than entries")]
    fn tiny_and_tall_budgets_count_every_entry_left_out(
        height: usize,
        rows: usize,
        shown: usize,
        footer: &str,
    ) {
        let kept = entries(HISTORY_LIMIT);
        let dropped = 501;
        let drawn = texts(&panel(&kept, dropped, 100, height), Colours::None);
        assert_budget("tiny and tall", &drawn, 100, height, rows, footer);
        if drawn.len() == rows && shown > 0 {
            for (i, entry) in kept[..shown].iter().enumerate() {
                let want = cut_words(&entry.text(), 84, Colours::None);
                assert!(
                    drawn[2 + i].ends_with(&want),
                    "height {height}: entry {i} is {:?}, want suffix {want:?}",
                    drawn[2 + i]
                );
            }
        }
    }

    #[test]
    fn a_zero_width_panel_is_empty() {
        let kept = entries(HISTORY_LIMIT);
        assert!(panel(&kept, 501, 0, 10).is_empty());
    }

    #[test_case(0, 0, ""; "no rows")]
    #[test_case(1, 1, ""; "one row")]
    #[test_case(2, 2, ""; "two rows")]
    #[test_case(3, 2, ""; "three rows")]
    #[test_case(10, 2, ""; "ten rows")]
    fn an_empty_history_leaves_nothing_uncounted(height: usize, rows: usize, footer: &str) {
        let drawn = texts(&panel(&[], 0, 100, height), Colours::None);
        assert_budget("empty", &drawn, 100, height, rows, footer);
    }

    #[test_case(0, 0, ""; "no rows")]
    #[test_case(1, 1, "+1 more"; "one row")]
    #[test_case(2, 2, "+1 more"; "two rows")]
    #[test_case(3, 3, ""; "three rows")]
    #[test_case(10, 3, ""; "ten rows")]
    fn one_entry_leaves_nothing_uncounted(height: usize, rows: usize, footer: &str) {
        let drawn = texts(&panel(&entries(1), 0, 100, height), Colours::None);
        assert_budget("one entry", &drawn, 100, height, rows, footer);
    }

    #[test_case(0, 0, ""; "no rows")]
    #[test_case(1, 1, "+3 more"; "one row")]
    #[test_case(2, 2, "+3 more"; "two rows")]
    #[test_case(3, 3, "+3 more"; "three rows")]
    #[test_case(4, 4, "+2 more"; "four rows")]
    #[test_case(5, 5, ""; "five rows")]
    #[test_case(10, 5, ""; "ten rows")]
    fn three_entries_leave_nothing_uncounted(height: usize, rows: usize, footer: &str) {
        let drawn = texts(&panel(&entries(3), 0, 100, height), Colours::None);
        assert_budget("three entries", &drawn, 100, height, rows, footer);
    }

    #[test_case(0, 0, ""; "no rows")]
    #[test_case(1, 1, "+7 more"; "one row")]
    #[test_case(2, 2, "+7 more"; "two rows")]
    #[test_case(3, 3, "+7 more"; "three rows")]
    #[test_case(10, 3, "+7 more"; "ten rows")]
    fn only_dropped_entries_leave_nothing_uncounted(height: usize, rows: usize, footer: &str) {
        let drawn = texts(&panel(&[], 7, 100, height), Colours::None);
        assert_budget("only dropped entries", &drawn, 100, height, rows, footer);
    }

    #[test_case(2 + HISTORY_LIMIT + 20, 2 + HISTORY_LIMIT + 1, HISTORY_LIMIT; "room for every kept entry")]
    #[test_case(2 + 50, 2 + 50, 49; "room for fifty rows")]
    fn the_panel_counts_every_entry_the_model_left_out(height: usize, rows: usize, shown: usize) {
        let warnings = (0..2 * HISTORY_LIMIT + 5)
            .map(|i| warning(&i.to_string()))
            .collect();
        let model = project(&journal(warnings));
        let s = model.snapshot();
        assert_eq!(s.history.len(), HISTORY_LIMIT);
        let total = s.history_dropped + s.history.len();
        assert!(total > 2 * HISTORY_LIMIT);
        let zone = TimeZone::UTC;
        let drawn = texts(&history(&scene(&s, &zone), 100, height), Colours::None);
        assert_eq!(drawn.len(), rows);
        assert_eq!(drawn.last(), Some(&format!("+{} more", total - shown)));
    }

    #[test]
    fn a_panel_with_room_for_every_entry_has_no_count_row() {
        let warnings = (0..4).map(|i| warning(&i.to_string())).collect();
        let model = project(&journal(warnings));
        let s = model.snapshot();
        let zone = TimeZone::UTC;
        let drawn = texts(&history(&scene_of(&s, &zone), 100, 40), Colours::None);
        assert_eq!(drawn.len(), 2 + s.history.len());
        assert!(!drawn.last().expect("a row").contains("more"), "{drawn:?}");
    }

    fn scene_of<'a>(
        s: &crate::model::Snapshot<'a>,
        zone: &'a TimeZone,
    ) -> super::super::super::scene::Scene<'a> {
        scene(s, zone)
    }

    #[test]
    fn a_row_is_the_local_minute_the_tag_and_the_sentence() {
        let kept = entries(1);
        let rows = texts(&panel(&kept, 0, 100, 4), Colours::None);
        assert_eq!(rows[2], "00:16  note     warning 000");
        assert!(
            rows[0].starts_with("WHAT HAPPENED")
                && rows[0].ends_with("newest first · l: last 400 events")
        );
        let narrow = texts(&panel(&kept, 0, 60, 4), Colours::None);
        assert!(narrow[0].ends_with("l: last 400"), "{}", narrow[0]);
        let cut = texts(&panel(&kept, 0, 26, 4), Colours::None);
        assert_eq!(cut[2], "00:16  note     warning...");
        assert!(line_width(&panel(&kept, 0, 26, 4)[2], Colours::None) <= 26);
    }

    #[test]
    fn the_alarm_in_a_sentence_is_red() {
        let entry = Entry::new(
            at(1000),
            Tag::Fail,
            crate::history::What::Failed(crate::history::Failed {
                subject: "core 03".to_owned(),
                verdict: crate::history::Verdict {
                    signal: Some(togi_machine::Signal::ComputationError),
                    when: None,
                    named: None,
                },
                reboot: None,
            }),
            Tone::Bad,
        );
        let model = Model::new();
        let mut s = model.snapshot();
        let kept = [entry];
        s.history = &kept;
        let zone = TimeZone::UTC;
        let scene = super::super::fixtures::scene(&s, &zone);
        let colourful = super::super::super::scene::Scene {
            colours: Colours::Full,
            ..scene
        };
        let rows = history(&colourful, 100, 4);
        let row = &rows[2];
        let alarm = row
            .spans
            .iter()
            .find(|span| span.content == "wrong result")
            .expect("the alarm span");
        assert_eq!(alarm.style, Paint::Bad.style(Colours::Full));
        let tag = row
            .spans
            .iter()
            .find(|span| span.content.starts_with("fail"))
            .expect("the tag span");
        assert_eq!(tag.style, Paint::Bad.style(Colours::Full));
        assert_eq!(
            plain(row, Colours::None),
            "00:16  fail     core 03 · wrong result"
        );
    }

    #[test]
    fn a_sentence_with_controls_is_escaped_at_output() {
        let kept = [Entry::line(
            at(1000),
            Tag::Note,
            "x\u{1b}[2Jy \u{9b}31m".to_owned(),
            Tone::Plain,
        )];
        let rows = texts(&panel(&kept, 0, 100, 4), Colours::None);
        assert_eq!(rows[2], "00:16  note     x\\x1b[2Jy \\u009b31m");
    }

    #[test_case(Tone::Plain, Paint::Text; "plain")]
    #[test_case(Tone::Good, Paint::Good; "good")]
    #[test_case(Tone::Bad, Paint::Bad; "bad")]
    #[test_case(Tone::Warn, Paint::Warn; "warn")]
    fn a_tag_is_painted_by_its_entry_s_tone(tone: Tone, paint: Paint) {
        let kept = [Entry::line(at(1000), Tag::Note, "x".to_owned(), tone)];
        let model = Model::new();
        let mut s = model.snapshot();
        s.history = &kept;
        let zone = TimeZone::UTC;
        let colourful = super::super::super::scene::Scene {
            colours: Colours::Full,
            ..scene(&s, &zone)
        };
        let rows = history(&colourful, 100, 4);
        assert_eq!(rows[2].spans[2].style, paint.style(Colours::Full));
    }
}
