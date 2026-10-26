//! Panels whole: the history count, the context panel's night and day, the trial bar and the between-trial machine
//! check.

use jiff::tz::TimeZone;
use togi_journal::{Mce, Payload};
use togi_machine::{BankType, CpuId};

use super::{Fixture, SIZES, append, bar, journal, later, main_screen, rows_of, text, words};
use crate::frames::hand::Staged;
use crate::frames::measure::measure;
use crate::frames::scene::Scene;
use crate::screen::Colours;
use crate::tests::kit::{core, warning};

fn region(rows: &[String], rect: ratatui::layout::Rect) -> Vec<String> {
    rows[usize::from(rect.y)..usize::from(rect.y + rect.height)]
        .iter()
        .map(|row| {
            row.chars()
                .skip(usize::from(rect.x))
                .take(usize::from(rect.width))
                .collect::<String>()
                .trim()
                .to_owned()
        })
        .collect()
}

#[test_case::test_case(120, 24; "120x24")]
#[test_case::test_case(120, 25; "120x25")]
#[test_case::test_case(160, 24; "160x24")]
#[test_case::test_case(160, 25; "160x25")]
#[test_case::test_case(240, 24; "240x24")]
#[test_case::test_case(240, 25; "240x25")]
fn a_history_too_short_for_its_entries_counts_every_one_it_leaves_out(width: u16, height: u16) {
    let warnings = (0..700)
        .map(|index| warning(&format!("warning {index}")))
        .collect();
    let fx = Fixture::of(&journal(warnings));
    let snapshot = fx.model.snapshot();
    let total = snapshot.history.len() + snapshot.history_dropped;
    assert!(total > 700);
    let sc = main_screen(width, height);
    let zone = TimeZone::UTC;
    let scene = Scene::new(snapshot, fx.now, &zone, Colours::None);
    let history = measure(&scene, &sc).history;
    assert!(history.height >= 1, "{width}x{height}");
    let (buf, _) = fx.draw(&sc, fx.now);
    let rows = region(&rows_of(&buf), history);
    let (last, entries) = rows.split_last().expect("a history row");
    let visible = entries
        .iter()
        .filter(|row| row.as_bytes().first().is_some_and(u8::is_ascii_digit))
        .count();
    assert_eq!(
        last,
        &format!("+{} more", total - visible),
        "{width}x{height}: the last visible history row counts every hidden entry ({rows:?})"
    );
}

#[test]
fn a_between_trial_machine_check_is_not_hidden() {
    let mut events = journal(Vec::new());
    let boot = events.last().expect("an event").boot.to_string();
    append(
        &mut events,
        Mce {
            cpu: CpuId::from(0),
            core: Some(core(0)),
            bank: 0,
            bank_type: BankType::LoadStore,
            corrected: true,
            monotonic_ns: None,
            from_boot: None,
            trial: None,
            between_trials: true,
            lines: vec!["between-trial hardware error".to_owned()],
        }
        .into(),
        60,
        &boot,
    );
    assert!(matches!(
        events.last().map(|event| &event.payload),
        Some(Payload::Mce(_))
    ));
    let fx = Fixture::of(&events);
    let said = words(&fx.main(240, 67).0);
    assert!(
        said.contains("between trials (recorded only)"),
        "watch hides a between-trial machine check: {said}"
    );
}

#[test]
fn the_trial_bar_shows_elapsed_without_colour_at_every_size() {
    let fx = Staged::Mapping.hand();
    for (width, height) in SIZES {
        let sc = main_screen(width, height);
        let bar_at = |now| {
            bar(&fx.draw(&sc, now).0)
                .unwrap_or_else(|| panic!("{width}x{height}: no trial bar row in the frame"))
        };
        let before = bar_at(fx.now);
        let after = bar_at(later(fx.now, 30));
        assert_ne!(
            before, after,
            "{width}x{height}: the bar is the same at two elapsed times"
        );
        let filled = |bar: &str| bar.chars().filter(|&c| c == '█').count();
        assert!(
            filled(&after) > filled(&before) && before.contains('░'),
            "{width}x{height}: elapsed cells must grow as █ over ░: {before:?} then {after:?}"
        );
    }
}

#[test_case::test_case(Staged::Mapping, "NIGHT since"; "a night")]
#[test_case::test_case(Staged::Day, "DAY"; "the day")]
fn a_short_dashboard_keeps_the_context_rules_and_every_ccd_table(staged: Staged, rule: &str) {
    let fx = staged.hand();
    for width in [120, 160] {
        for height in 33..=60 {
            let (buf, _) = fx.main(width, height);
            let shown = text(&buf);
            assert!(
                shown.contains(rule),
                "{width}x{height}: the context lost its {rule:?} rule:\n{shown}"
            );
            for table in ["CCD 0", "CCD 1"] {
                assert!(
                    shown.contains(table),
                    "{width}x{height}: {table} is missing:\n{shown}"
                );
            }
        }
    }
}

#[test]
fn the_context_rectangle_holds_the_whole_night_at_the_largest_size() {
    let fx = Staged::Mapping.hand();
    let sc = main_screen(240, 67);
    let (buf, _) = fx.draw(&sc, fx.now);
    let zone = TimeZone::UTC;
    let scene = Scene::new(fx.snapshot(), fx.now, &zone, Colours::None);
    let context = region(&rows_of(&buf), measure(&scene, &sc).context);
    for want in [
        "NIGHT since",
        "task",
        "mapping",
        "shares",
        "DAY",
        "hold",
        "firmware",
    ] {
        assert!(
            context.iter().any(|row| row.starts_with(want)),
            "the context lacks {want:?}: {context:#?}"
        );
    }
}
