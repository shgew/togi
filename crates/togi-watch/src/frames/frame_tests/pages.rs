//! Where each page puts its rows: the title row every page but the reference Main carries, Main's exact rows at
//! 160 by 45, its numeric form under that and its scrolled body under 80 by 30.

use jiff::tz::TimeZone;
use test_case::test_case;
use togi_journal::TrialProgress;
use togi_watchtest::Scenario;

use super::{Fixture, append};
use crate::frames::dash::core_table;
use crate::frames::fit::plain;
use crate::frames::layout::SizeClass;
use crate::frames::scene::Scene;
use crate::frames::testing::{rows_of, screen};
use crate::screen::{Colours, Screen, View};
use crate::tests::kit::{Script, Start, readback};

const EXPLANATION: &str =
    "white=tune · red point=recorded failure · gap=counts shallower than solo";

fn frame_rows(fx: &Fixture, view: View, (width, height): (u16, u16), scroll: usize) -> Vec<String> {
    let sc = Screen {
        scroll,
        ..screen(view, width, height, Colours::None, true)
    };
    rows_of(&fx.draw(&sc, fx.now).0)
}

fn title_of(view: View) -> String {
    format!(
        "{} · {}/{} · < {} | {} >",
        view.name().to_uppercase(),
        view.number(),
        View::ALL.len(),
        view.prev().name(),
        view.next().name()
    )
}

#[test_case(Scenario::Checking; "checking")]
#[test_case(Scenario::Stopped; "stopped")]
#[test_case(Scenario::Hunt; "hunt")]
fn every_page_titles_itself_with_its_place_and_its_neighbours_even_scrolled(scenario: Scenario) {
    let fx = Fixture::scenario(scenario);
    for view in View::ALL {
        for size in [(160, 45), (120, 33), (80, 30), (80, 24), (79, 29)] {
            for scroll in [0, usize::MAX] {
                let rows = frame_rows(&fx, view, size, scroll);
                let title = rows[2].trim();
                let gauges =
                    view == View::Main && SizeClass::of(size.0, size.1) != SizeClass::Compact;
                if gauges {
                    assert_eq!(
                        title, "",
                        "{scenario} Main {size:?}: row 2 is blank at the reference size"
                    );
                } else {
                    assert_eq!(
                        title,
                        title_of(view),
                        "{scenario} {view:?} {size:?} scrolled {scroll}"
                    );
                }
            }
        }
    }
}

#[test_case(32, "CORES · 2/9 · <Main|Cycle>"; "the compact neighbours")]
#[test_case(27, "CORES · 2/9"; "no neighbours")]
#[test_case(14, "CORES · 2/9"; "the place only")]
#[test_case(13, "CORES"; "the name only")]
fn a_narrow_title_gives_up_whole_clauses_and_keeps_the_page_name(width: u16, want: &str) {
    let fx = Fixture::scenario(Scenario::Checking);
    let rows = frame_rows(&fx, View::Cores, (width, 12), 0);
    assert_eq!(rows[2].trim(), want, "{width} columns");
}

#[test]
fn main_at_160_by_45_has_the_designs_exact_rows_and_margins() {
    let fx = Fixture::scenario(Scenario::Checking);
    let rows = frame_rows(&fx, View::Main, (160, 45), 0);
    assert_eq!(rows.len(), 44);
    for row in [2, 5, 8, 36] {
        assert_eq!(rows[row].trim(), "", "row {row} is whitespace");
    }
    assert_eq!(rows[35].trim(), EXPLANATION);
    assert_eq!(rows[37].trim(), "WHAT HAPPENED");
    for (y, row) in rows.iter().enumerate() {
        let cells: Vec<char> = row.chars().collect();
        assert_eq!(cells.len(), 159, "row {y}");
        assert!(
            cells[0] == ' ' && cells[158] == ' ',
            "row {y} writes into a margin column: {row:?}"
        );
    }
    for (y, row) in rows.iter().enumerate().take(35).skip(9) {
        let cells: Vec<char> = row.chars().collect();
        assert!(
            cells[78..81].iter().all(|&c| c == ' '),
            "row {y} writes into the gap between the CCDs: {row:?}"
        );
    }
    for y in [0, 1, 9, 43] {
        assert_ne!(
            rows[y].trim(),
            "",
            "row {y} is shared chrome or the gauge heading"
        );
    }
}

#[test]
fn one_full_ccd_at_160_by_45_shows_every_core_in_the_numeric_table_over_the_gauges_rows() {
    let cores: Vec<(u8, u8)> = (0..8).map(|id| (id, 0)).collect();
    let tune = |id: u8| -10 - 2 * i64::from(id);
    let now = |id: u8| -30 - i64::from(id);
    let starts: Vec<Start> = (0..8)
        .map(|id| Start::at_limit(tune(id), tune(id) - 1))
        .collect();
    let mut script = Script::new(&cores, &starts);
    for id in 0..8 {
        script.append(readback(id, now(id)), &[]);
    }
    let fx = Fixture::of(&script.events);
    let rows = frame_rows(&fx, View::Main, (160, 45), 0);
    assert_eq!(rows.len(), 44);
    for (y, row) in rows.iter().enumerate() {
        let cells: Vec<char> = row.chars().collect();
        assert_eq!(cells.len(), 159, "row {y}");
        assert!(
            cells[0] == ' ' && cells[158] == ' ',
            "row {y} writes into a margin column: {row:?}"
        );
    }
    assert!(
        rows[9].starts_with(" CCD core tune  now"),
        "the table's heading: {:?}",
        rows[9]
    );
    for id in 0..8 {
        let row = &rows[10 + usize::from(id)];
        let numbers = format!(
            " {:>3} {:>4} {:>4} {:>4} ",
            0,
            format!("{id:02}"),
            tune(id),
            now(id)
        );
        assert!(row.starts_with(&numbers), "core {id}: {row:?}");
    }
    for (y, row) in rows.iter().enumerate().take(37).skip(18) {
        assert_eq!(row.trim(), "", "row {y} is under the table");
    }
    assert_eq!(rows[37].trim(), "WHAT HAPPENED");
}

#[test_case((159, 45); "one column under")]
#[test_case((160, 44); "one row under")]
fn one_cell_under_the_reference_size_is_the_numeric_main(size: (u16, u16)) {
    let fx = Fixture::scenario(Scenario::Checking);
    let rows = frame_rows(&fx, View::Main, size, 0);
    assert!(
        rows[2].starts_with(" MAIN · 1/9"),
        "{size:?}: Main names its page: {:?}",
        rows[2]
    );
    let shown = rows.join("\n");
    assert!(
        !shown.contains("white=tune"),
        "{size:?} draws gauges:\n{shown}"
    );
    for word in ["tune", "solo", "fail"] {
        assert!(
            rows[9].contains(word),
            "{size:?} heading lacks {word:?}: {:?}",
            rows[9]
        );
    }
    for (y, row) in rows.iter().enumerate().take(26).skip(10) {
        assert_ne!(row.trim(), "", "{size:?} core row {y}");
    }
    assert!(
        rows[26].trim_start().starts_with("WHAT HAPPENED"),
        "{size:?}: {:?}",
        rows[26]
    );
}

#[test]
fn main_at_80_by_30_has_the_numeric_designs_rows() {
    let fx = Fixture::scenario(Scenario::Checking);
    let rows = frame_rows(&fx, View::Main, (80, 30), 0);
    assert_eq!(rows.len(), 29);
    assert_eq!(rows[2].trim(), title_of(View::Main));
    for word in ["tune", "now", "solo", "gap", "fail", "role"] {
        assert!(
            rows[9].contains(word),
            "heading lacks {word:?}: {:?}",
            rows[9]
        );
    }
    for (y, row) in rows.iter().enumerate().take(26).skip(10) {
        assert_ne!(row.trim(), "", "core row {y}");
    }
    assert!(
        rows[26].trim_start().starts_with("WHAT HAPPENED"),
        "one summary row, named: {:?}",
        rows[26]
    );
    for row in [5, 8, 27] {
        assert_eq!(rows[row].trim(), "", "row {row} is whitespace");
    }
    assert_ne!(rows[28].trim(), "", "the footer");
}

#[test]
fn under_80_by_30_main_scrolls_and_every_core_is_reachable() {
    let fx = Fixture::scenario(Scenario::Checking);
    for size in [(79, 29), (80, 29), (79, 30), (60, 20), (40, 12)] {
        let top = frame_rows(&fx, View::Main, size, 0);
        let mut sc = screen(View::Main, size.0, size.1, Colours::None, true);
        let drawn = fx.draw(&sc, fx.now).1;
        assert!(
            drawn.scroll > 0,
            "{size:?}: the numeric body does not fit and must scroll"
        );
        assert!(
            top.join("\n").contains("compact · scroll for all cores"),
            "{size:?} does not say that it scrolls:\n{}",
            top.join("\n")
        );
        let zone = TimeZone::UTC;
        let scene = Scene::new(fx.model.snapshot(), fx.now, &zone, Colours::None);
        let inner = usize::from(size.0.saturating_sub(1)).saturating_sub(2 + 2);
        let table: Vec<String> = core_table(&scene, inner)
            .iter()
            .map(|line| plain(line).trim().to_owned())
            .filter(|line| !line.is_empty())
            .collect();
        assert!(table.len() > 16, "{size:?}: a heading and sixteen cores");
        let mut seen = vec![false; table.len()];
        for scroll in 0..=drawn.scroll {
            sc.scroll = scroll;
            let (buf, again) = fx.draw(&sc, fx.now);
            assert_eq!(
                again.scroll, drawn.scroll,
                "{size:?}: the extent does not move"
            );
            let rows = rows_of(&buf);
            assert!(
                rows[2].trim().starts_with("MAIN"),
                "{size:?}: the title is sticky"
            );
            for (n, line) in table.iter().enumerate() {
                seen[n] |= rows.iter().any(|row| row.contains(line.as_str()));
            }
        }
        assert!(
            seen.iter().all(|&shown| shown),
            "{size:?}: unreachable rows {:?}",
            table
                .iter()
                .zip(&seen)
                .filter(|(_, shown)| !**shown)
                .map(|(line, _)| line)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn the_unread_journal_warning_takes_mains_own_row_and_the_footer_elsewhere() {
    let mut fx = Fixture::scenario(Scenario::Checking);
    fx.model
        .set_skipped(Some("2 lines skipped (unknown event kind)".to_owned()));
    let main = frame_rows(&fx, View::Main, (160, 45), 0);
    assert_eq!(main[42].trim(), "2 lines skipped (unknown event kind)");
    assert!(!main[43].contains("skipped"), "{:?}", main[43]);
    let cores = frame_rows(&fx, View::Cores, (160, 45), 0);
    assert!(cores[43].contains("skipped"), "{:?}", cores[43]);
}

#[test]
fn the_log_page_folds_a_trials_passed_self_tests_into_one_row() {
    let mut events = Scenario::Search.events().expect("the scenario's journal");
    let trial = Fixture::of(&events)
        .model
        .snapshot()
        .trial
        .expect("a started trial")
        .id
        .clone();
    let boot = events.last().expect("an event").boot.to_string();
    for size in ["4K", "8K"] {
        append(
            &mut events,
            TrialProgress {
                trial: trial.clone(),
                detail: format!("self-test {size} passed"),
                signal: None,
                core: None,
            }
            .into(),
            1,
            &boot,
        );
    }
    let rows = frame_rows(&Fixture::of(&events), View::Log, (240, 67), usize::MAX);
    let folded = format!("trial {trial}: 2 self-test checks passed");
    assert!(
        rows.iter().any(|row| row.trim_end().ends_with(&folded)),
        "{rows:#?}"
    );
}
