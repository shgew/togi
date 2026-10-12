//! What happened: the history panel.

use ratatui::text::{Line, Span};

use super::super::colours::Paint;
use super::super::fit::{cut_styled, rule};
use super::super::scene::Scene;
use super::table::cell;
use crate::history::{Entry, LOG_LIMIT};

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
    let mut shown = entries.len().min(room);
    if scene.s.history_dropped > 0 || entries.len() > room {
        shown = entries.len().min(room.saturating_sub(1));
    }
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
    out
}

fn row(scene: &Scene<'_>, entry: &Entry, width: usize) -> Line<'static> {
    let (before, alarm, after) = entry.sentence();
    let sentence = Line::from(vec![
        scene.paint(Paint::Text, before),
        scene.paint(Paint::Bad, alarm),
        scene.paint(Paint::Text, after),
    ]);
    let mut spans = vec![
        scene.paint(Paint::Grey, scene.wall_minute(entry.at)),
        Span::raw("  "),
        scene.paint(Paint::from(entry.tone), format!("{:<8}", entry.tag.word())),
        Span::raw(" "),
    ];
    spans.extend(cut_styled(&sentence, width.saturating_sub(PREFIX)).spans);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;
    use ratatui::text::Line;

    use super::super::super::colours::Paint;
    use super::super::super::fit::{cut_words, line_width, plain};
    use super::super::fixtures::{at, scene, texts};
    use super::history;
    use crate::history::{Entry, HISTORY_LIMIT};
    use crate::model::Model;
    use crate::screen::Colours;
    use crate::tests::kit::{Script, warning};
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

    #[test]
    fn tiny_and_tall_budgets_count_every_entry_left_out() {
        let kept = entries(HISTORY_LIMIT);
        let dropped = 501;
        for (height, rows, shown, footer) in [
            (0, 0, 0, ""),
            (1, 1, 0, "+701 more"),
            (2, 2, 0, "+701 more"),
            (3, 3, 0, "+701 more"),
            (10, 10, 7, "+694 more"),
            (202, 202, 199, "+502 more"),
            (203, 203, 200, "+501 more"),
            (222, 203, 200, "+501 more"),
        ] {
            let drawn = texts(&panel(&kept, dropped, 100, height));
            assert_budget("tiny and tall", &drawn, 100, height, rows, footer);
            if drawn.len() == rows && shown > 0 {
                for (i, entry) in kept[..shown].iter().enumerate() {
                    let want = cut_words(&entry.text, 84);
                    assert!(
                        drawn[2 + i].ends_with(&want),
                        "height {height}: entry {i} is {:?}, want suffix {want:?}",
                        drawn[2 + i]
                    );
                }
            }
        }
        assert!(panel(&kept, dropped, 0, 10).is_empty());
    }

    #[test]
    fn empty_and_small_budgets_leave_nothing_uncounted() {
        let one = entries(1);
        let three = entries(3);
        for (name, kept, dropped, budgets) in [
            (
                "empty",
                &[][..],
                0,
                vec![(0, 0, ""), (1, 1, ""), (2, 2, ""), (3, 2, ""), (10, 2, "")],
            ),
            (
                "one entry",
                &one,
                0,
                vec![
                    (0, 0, ""),
                    (1, 1, "+1 more"),
                    (2, 2, "+1 more"),
                    (3, 3, ""),
                    (10, 3, ""),
                ],
            ),
            (
                "three entries",
                &three,
                0,
                vec![
                    (0, 0, ""),
                    (1, 1, "+3 more"),
                    (2, 2, "+3 more"),
                    (3, 3, "+3 more"),
                    (4, 4, "+2 more"),
                    (5, 5, ""),
                    (10, 5, ""),
                ],
            ),
            (
                "only dropped entries",
                &[][..],
                7,
                vec![
                    (0, 0, ""),
                    (1, 1, "+7 more"),
                    (2, 2, "+7 more"),
                    (3, 3, "+7 more"),
                    (10, 3, "+7 more"),
                ],
            ),
        ] {
            for (height, rows, footer) in budgets {
                let drawn = texts(&panel(kept, dropped, 100, height));
                assert_budget(name, &drawn, 100, height, rows, footer);
            }
        }
    }

    #[test]
    fn the_panel_counts_every_entry_the_model_left_out() {
        let mut script = Script::new(&[(0, 0)], &[]);
        for i in 0..2 * HISTORY_LIMIT + 5 {
            script.append(warning(&i.to_string()), &[]);
        }
        let model = script.model();
        let s = model.snapshot();
        assert_eq!(s.history.len(), HISTORY_LIMIT);
        let total = s.history_dropped + s.history.len();
        assert!(total > 2 * HISTORY_LIMIT);
        let zone = TimeZone::UTC;
        let scene = scene(&s, &zone);
        for (height, rows, left_out) in [
            (
                2 + HISTORY_LIMIT + 20,
                2 + HISTORY_LIMIT + 1,
                s.history_dropped,
            ),
            (2 + 50, 2 + 50, total - 49),
        ] {
            let drawn = texts(&history(&scene, 100, height));
            assert_eq!(drawn.len(), rows);
            assert_eq!(drawn.last(), Some(&format!("+{left_out} more")));
        }
        let mut short = Script::new(&[(0, 0)], &[]);
        for i in 0..4 {
            short.append(warning(&i.to_string()), &[]);
        }
        let model = short.model();
        let s = model.snapshot();
        let zone = TimeZone::UTC;
        let drawn = texts(&history(&scene_of(&s, &zone), 100, 40));
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
        let rows = texts(&panel(&kept, 0, 100, 4));
        assert_eq!(rows[2], "00:16  note     warning 000");
        assert!(
            rows[0].starts_with("WHAT HAPPENED")
                && rows[0].ends_with("newest first · l: last 400 events")
        );
        let narrow = texts(&panel(&kept, 0, 60, 4));
        assert!(narrow[0].ends_with("l: last 400"), "{}", narrow[0]);
        let cut = texts(&panel(&kept, 0, 26, 4));
        assert_eq!(cut[2], "00:16  note     warning...");
        assert!(line_width(&panel(&kept, 0, 26, 4)[2]) <= 26);
    }

    #[test]
    fn the_alarm_in_a_sentence_is_red() {
        let mut entry = Entry::line(
            at(1000),
            Tag::Fail,
            "core 03 failed: boom".to_owned(),
            Tone::Bad,
        );
        entry.alarm = Some("boom".to_owned());
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
            .find(|span| span.content == "boom")
            .expect("the alarm span");
        assert_eq!(alarm.style, Paint::Bad.style(Colours::Full));
        let tag = row
            .spans
            .iter()
            .find(|span| span.content.starts_with("fail"))
            .expect("the tag span");
        assert_eq!(tag.style, Paint::Bad.style(Colours::Full));
        assert_eq!(plain(row), "00:16  fail     core 03 failed: boom");
    }

    #[test]
    fn a_sentence_with_controls_is_escaped_at_output() {
        let kept = [Entry::line(
            at(1000),
            Tag::Note,
            "x\u{1b}[2Jy \u{9b}31m".to_owned(),
            Tone::Plain,
        )];
        let rows = texts(&panel(&kept, 0, 100, 4));
        assert_eq!(rows[2], "00:16  note     x\\x1b[2Jy \\u009b31m");
    }

    #[test]
    fn a_tag_is_painted_by_its_entry_s_tone() {
        for (tone, paint) in [
            (Tone::Plain, Paint::Text),
            (Tone::Good, Paint::Good),
            (Tone::Bad, Paint::Bad),
            (Tone::Warn, Paint::Hunt),
            (Tone::Hunt, Paint::Hunt),
            (Tone::Combo, Paint::Combination),
        ] {
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
            assert_eq!(
                rows[2].spans[2].style,
                paint.style(Colours::Full),
                "{tone:?}"
            );
        }
    }
}
