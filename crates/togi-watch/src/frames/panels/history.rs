//! What happened: the history panel.

use ratatui::text::{Line, Span};

use super::super::colours::Paint;
use super::super::fit::{Clause, Form, cut_styled, fit_clauses, line_width, rule};
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
    let sentence = if entry.verdict.is_some() {
        verdict_sentence(scene, entry, room)
    } else {
        let (before, alarm, after) = entry.sentence();
        cut_styled(
            &Line::from(vec![
                scene.paint(Paint::Text, before),
                scene.paint(Paint::Bad, alarm),
                scene.paint(Paint::Text, after),
            ]),
            room,
        )
    };
    spans.extend(sentence.spans);
    Line::from(spans)
}

/// The sentence of a failed trial in `room` cells. What the failure was and whom it names are reserved first;
/// the timing gives way before anything else, and the subject is cut to what is left, so a long subject never
/// takes the attribution's cells.
fn verdict_sentence(scene: &Scene<'_>, entry: &Entry, room: usize) -> Line<'static> {
    let Some(verdict) = &entry.verdict else {
        return Line::default();
    };
    let sep = || Line::from(scene.paint(Paint::Text, " · ".to_owned()));
    let signal_paint = if entry.alarm.is_some() {
        Paint::Bad
    } else {
        Paint::Text
    };
    let signal = Line::from(scene.paint(signal_paint, verdict.signal.clone()));
    let named = verdict
        .named
        .as_ref()
        .map(|named| Line::from(scene.paint(Paint::Text, named.clone())));
    let reserved: usize = line_width(&signal)
        + named
            .as_ref()
            .map_or(0, |named| line_width(named) + line_width(&sep()));
    let subject_room = room.saturating_sub(reserved + line_width(&sep()));
    let subject_text = scene.paint(Paint::Text, entry.text.clone());
    let subject_cut = cut_styled(&Line::from(subject_text.clone()), subject_room);
    let mut subject = vec![Form::new(Line::from(subject_text), 1)];
    subject.push(if line_width(&subject_cut) > 0 {
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
    fit_clauses(room, &clauses)
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

    /// The history pane of the Medium class at 160x45: 75 cells, 16 of them before the sentence.
    mod medium {
        use togi_journal::{Outcome, TrialEnd, TrialId, TrialIntent};
        use togi_machine::{Condition, Regime, Signal};

        use super::super::PREFIX;
        use super::{cut_words, panel, texts};
        use crate::model::Count;
        use crate::tests::kit::{Hist, core, end_payload, intent, nz};

        const WIDTH: usize = 75;
        const SENTENCE: usize = WIDTH - PREFIX;
        const ALL: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7];
        const SPLIT: &[u8] = &[0, 1, 2, 5, 6, 7];

        fn eight() -> Hist {
            Hist::new(&(0..8).map(|id| (id, 0)).collect::<Vec<_>>())
        }

        /// Folds a failed trial of R7 on the cores `cores`; `count` is its place among the part's trials.
        fn fail(
            h: &mut Hist,
            cores: &[u8],
            count: Option<(u32, u32)>,
            shape: impl FnOnce(&mut TrialIntent),
            end: impl FnOnce(&mut TrialEnd),
        ) {
            let id = format!("t{}", h.count);
            let mut plan = intent(&id, Regime::R7, Condition::Together, &[0; 8], cores);
            shape(&mut plan);
            h.push(plan.into());
            let trial = TrialId::from(id);
            let mut over = end_payload(&trial, Outcome::Failure, Some(Signal::ComputationError));
            end(&mut over);
            if let Some((index, of)) = count {
                h.facts.counts.insert(
                    trial,
                    Count {
                        passed: index - 1,
                        index,
                        of,
                    },
                );
            }
            h.push(over.into());
        }

        fn crash_at(second: u32) -> impl FnOnce(&mut TrialEnd) {
            move |end| {
                end.signal = Some(Signal::Crash);
                end.last_sample_s = Some(second);
            }
        }

        fn early(h: &mut Hist) {
            fail(h, ALL, Some((2, 5)), |i| i.rerun = true, crash_at(10));
        }

        fn late(h: &mut Hist) {
            fail(h, ALL, Some((2, 5)), |i| i.rerun = true, crash_at(110));
        }

        fn midway(h: &mut Hist) {
            fail(h, SPLIT, Some((1, 5)), |i| i.rerun = true, crash_at(60));
        }

        fn partial(h: &mut Hist) {
            fail(
                h,
                SPLIT,
                Some((1, 3)),
                |i| i.cycle = Some(nz(1)),
                crash_at(10),
            );
        }

        fn wrong(h: &mut Hist) {
            fail(
                h,
                &[0, 1, 2, 4, 5, 6, 7],
                None,
                |i| i.rerun = true,
                |end| end.core = Some(core(4)),
            );
        }

        fn parked(h: &mut Hist) {
            fail(
                h,
                ALL,
                None,
                |i| {
                    i.rerun = true;
                    i.condition = Condition::Parked;
                },
                |_| {},
            );
        }

        /// The sentence the pane draws for the newest entry at `width`.
        fn drawn(h: &Hist, width: usize) -> String {
            let (kept, _) = h.facts.history.recent();
            let rows = texts(&panel(&kept, 0, width, 12));
            rows[2].chars().skip(PREFIX).collect()
        }

        /// Checks what the pane keeps of the newest entry against what the blind cut it replaces would have
        /// kept, and returns the sentence the pane draws.
        fn assert_kept(h: &Hist, signal: &str, attribution: &str) -> String {
            let (kept, _) = h.facts.history.recent();
            let full = kept[0].full_text().into_owned();
            assert!(
                full.contains(signal) && full.ends_with(attribution),
                "{full:?}"
            );
            let blind = cut_words(&full, SENTENCE);
            assert!(
                !blind.contains(attribution),
                "a blind cut {blind:?} loses {attribution:?}, which is the defect"
            );
            let shown = drawn(h, WIDTH);
            assert!(shown.chars().count() <= SENTENCE, "{shown:?}");
            assert!(
                shown.contains(signal) && shown.ends_with(attribution),
                "{shown:?}"
            );
            shown
        }

        #[test]
        fn an_early_rerun_crash_keeps_no_core_named() {
            let mut h = eight();
            early(&mut h);
            assert_eq!(
                assert_kept(&h, "rebooted", "no core named"),
                "rerun of R7 all-core on 00-07 · rebooted · no core named"
            );
            let (kept, _) = h.facts.history.recent();
            assert_eq!(
                kept[0].full_text(),
                "rerun of R7 all-core on 00-07 · rebooted · early in trial 2 of 5 · no core named"
            );
        }

        #[test]
        fn a_late_rerun_crash_keeps_no_core_named() {
            let mut h = eight();
            late(&mut h);
            assert_eq!(
                assert_kept(&h, "rebooted", "no core named"),
                "rerun of R7 all-core on 00-07 · rebooted · no core named"
            );
        }

        #[test]
        fn a_midway_rerun_crash_keeps_no_core_named_and_cuts_its_subject() {
            let mut h = eight();
            midway(&mut h);
            let shown = assert_kept(&h, "rebooted", "no core named");
            assert!(shown.starts_with("rerun of R7 all-core on"), "{shown:?}");
            assert!(shown.contains("... · rebooted"), "{shown:?}");
        }

        #[test]
        fn a_partial_ccd_crash_keeps_its_part_and_no_core_named() {
            let mut h = eight();
            partial(&mut h);
            assert_eq!(
                assert_kept(&h, "rebooted", "no core named"),
                "R7 partial CCD 0 on 00-02 05-07 · rebooted · no core named"
            );
        }

        #[test]
        fn a_wrong_result_keeps_the_core_it_names() {
            let mut h = eight();
            wrong(&mut h);
            let shown = assert_kept(&h, "wrong result", "core 04 named");
            assert!(shown.starts_with("rerun of R7 all-core on"), "{shown:?}");
        }

        #[test]
        fn a_long_subject_never_takes_the_attribution_cells() {
            let mut h = eight();
            parked(&mut h);
            let shown = assert_kept(&h, "wrong result", "no core named");
            assert!(shown.starts_with("rerun of R7"), "{shown:?}");
            let bare = "wrong result · no core named";
            let bare_width = bare.chars().count();
            assert_eq!(drawn(&h, PREFIX + bare_width), bare);
            for width in PREFIX + bare_width..WIDTH {
                let shown = drawn(&h, width);
                assert!(shown.ends_with("no core named"), "{width}: {shown:?}");
                assert!(shown.contains("wrong result"), "{width}: {shown:?}");
            }
        }

        #[test]
        fn the_medium_history_pane() {
            let mut h = eight();
            early(&mut h);
            late(&mut h);
            midway(&mut h);
            partial(&mut h);
            wrong(&mut h);
            parked(&mut h);
            let (kept, _) = h.facts.history.recent();
            let rows = texts(&panel(&kept, 0, WIDTH, 12));
            let (_, expected) = include_str!("snapshots/history-attribution-75.snap")
                .strip_prefix("---\n")
                .and_then(|snapshot| snapshot.split_once("\n---\n"))
                .expect("a committed snapshot has an insta metadata header");
            assert_eq!(rows.join("\n"), expected.trim_end_matches('\n'));
        }
    }
}
