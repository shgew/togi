//! Detail pages and panels whole: the cycle, R7, hunt and BIOS pages, the trial bar and a partial trial.

use jiff::tz::TimeZone;
use togi_journal::{Mce, Payload};
use togi_machine::{BankType, CpuId, Regime, WorkloadId, workloads};
use togi_watchtest::Scenario;

use super::{Fixture, SIZES, append, bar, later, main_screen, some_time, text};
use crate::frames::fit::plain;
use crate::frames::layout::SizeClass;
use crate::frames::panels::{history_page, page_lines};
use crate::frames::scene::Scene;
use crate::model::Model;
use crate::screen::{Colours, Journal, View};
use crate::tests::kit::core;
use crate::words::workload_display;

#[test]
fn a_between_trial_machine_check_is_not_hidden() {
    let mut events = Scenario::Between.events().expect("the scenario's journal");
    let boot = events.last().expect("an event").boot.to_string();
    append(
        &mut events,
        Mce {
            cpu: CpuId(0),
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
    let snapshot = fx.model.snapshot();
    let zone = TimeZone::UTC;
    let scene = Scene::new(snapshot, fx.now, &zone, Colours::None);
    let said = history_page(&scene, 120)
        .iter()
        .map(plain)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        said.contains("between trials (recorded only)"),
        "the history page hides a between-trial machine check: {said}"
    );
}

#[test]
fn the_trial_bar_shows_elapsed_without_colour_at_every_size() {
    let fx = Fixture::scenario(Scenario::Search);
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

#[test]
fn a_partial_trial_is_named_by_its_ordinary_evidence() {
    let fx = Fixture::scenario(Scenario::Checking);
    let mut trial = fx.model.snapshot().trial.expect("a trial").clone();
    trial.partial = true;
    let mut snapshot = fx.model.snapshot();
    snapshot.trial = Some(&trial);
    let journal = Journal::Ready {
        snapshot,
        log: snapshot.log(),
    };
    for (width, height) in SIZES {
        let (buf, _) = super::draw_journal(
            &journal,
            &main_screen(width, height),
            fx.now,
            &TimeZone::UTC,
        );
        let shown = text(&buf);
        assert!(shown.contains("PARTIAL"), "{width}x{height}:\n{shown}");
        assert!(!shown.contains("RECORD ONLY"), "{width}x{height}:\n{shown}");
    }
}

fn page(fx: &Fixture, view: View, width: usize, class: SizeClass) -> Vec<String> {
    let snapshot = fx.model.snapshot();
    let zone = TimeZone::UTC;
    let scene = Scene::new(snapshot, fx.now, &zone, Colours::None);
    page_lines(&scene, view, width, class)
        .iter()
        .map(plain)
        .collect()
}

const CLASSES: [(usize, SizeClass); 3] = [
    (60, SizeClass::Compact),
    (100, SizeClass::Medium),
    (150, SizeClass::Wide),
];

#[test]
fn the_cycle_page_lists_the_search_turns_while_solo_limits_are_searched() {
    let fx = Fixture::scenario(Scenario::Search);
    for (width, class) in CLASSES {
        let rows = page(&fx, View::Cycle, width, class);
        assert!(rows[0].starts_with("TURNS"), "{width}: {rows:?}");
        assert!(rows.iter().any(|row| row.contains("search:")), "{rows:?}");
        assert!(rows.iter().all(|row| !row.contains(" more")), "{rows:?}");
    }
}

#[test]
fn the_cycle_page_lists_the_whole_checklist_with_the_open_r7_parts() {
    let fx = Fixture::scenario(Scenario::Checking);
    for (width, class) in CLASSES {
        let rows = page(&fx, View::Cycle, width, class);
        assert!(rows[0].starts_with("CYCLE"), "{width}: {rows:?}");
        let steps = rows
            .iter()
            .filter(|row| row.contains("R1 ") || row.contains("R2 ") || row.contains("R7 "))
            .count();
        assert!(steps >= 3, "{width}: {rows:?}");
        assert!(
            rows.iter()
                .all(|row| !row.contains(" more") || row.contains("more may follow"))
        );
        assert!(
            rows.iter().any(|row| row.contains("known part")),
            "{width}: an open R7 step counts its known parts: {rows:?}"
        );
    }
}

#[test]
fn the_cycle_page_shows_the_open_phase_2_round() {
    let fx = Fixture::scenario(Scenario::Round);
    let rows = page(&fx, View::Cycle, 100, SizeClass::Medium);
    assert!(rows[0].starts_with("PHASE 2 · ROUND"), "{rows:?}");
    assert!(rows.iter().any(|row| row.contains("gap ")), "{rows:?}");
    assert!(rows.iter().any(|row| row == "CHECKS"), "{rows:?}");
}

#[test]
fn a_stopped_session_draws_no_running_marker_on_the_cycle_page() {
    let fx = Fixture::scenario(Scenario::Stopped);
    for (width, class) in CLASSES {
        let rows = page(&fx, View::Cycle, width, class);
        assert!(rows.iter().all(|row| !row.contains('►')), "{rows:?}");
    }
}

#[test]
fn the_r7_page_explains_the_rail_and_the_evidence() {
    let fx = Fixture::scenario(Scenario::Checking);
    for (width, class) in CLASSES {
        let rows = page(&fx, View::R7, width, class);
        let shown = rows.join("\n");
        assert!(rows[0].starts_with("R7 EVIDENCE"), "{width}: {shown}");
        for want in [
            "share one voltage rail",
            "per offset order",
            "not a guarantee",
        ] {
            assert!(shown.contains(want), "{width} lacks {want:?}:\n{shown}");
        }
        for name in r7_names() {
            assert!(
                shown.contains(&name),
                "{width} lacks workload {name}:\n{shown}"
            );
        }
        assert!(shown.contains("CCD 0"), "{shown}");
    }
}

#[test]
fn a_running_r7_trial_gives_its_forecast_neutral_conditional_wording() {
    let fx = Fixture::scenario(Scenario::Checking);
    let shown = page(&fx, View::R7, 100, SizeClass::Medium).join("\n");
    assert!(shown.contains("any outcome is conditional"), "{shown}");
}

fn r7_names() -> Vec<String> {
    (0..workloads(Regime::R7).len())
        .filter_map(|index| WorkloadId::new(Regime::R7, index))
        .map(|id| workload_display(id).to_owned())
        .collect()
}

#[test]
fn the_hunts_page_leads_with_the_cause_of_the_running_hunt() {
    let fx = Fixture::scenario(Scenario::Hunt);
    for (width, class) in CLASSES {
        let rows = page(&fx, View::Hunts, width, class);
        assert!(rows[0].starts_with("HUNT "), "{width}: {rows:?}");
        assert!(rows[2].starts_with("cause"), "{width}: {rows:?}");
    }
    let probes = Fixture::scenario(Scenario::MemberProbe);
    let rows = page(&probes, View::Hunts, 150, SizeClass::Wide);
    assert!(rows.iter().any(|row| row.contains("member")), "{rows:?}");
}

#[test]
fn pages_with_nothing_to_show_say_so_in_a_sentence() {
    let search = Fixture::scenario(Scenario::Search);
    let hunts = page(&search, View::Hunts, 100, SizeClass::Medium).join(" ");
    assert!(hunts.contains("No hunt is running"), "{hunts}");
    let r7 = page(&search, View::R7, 100, SizeClass::Medium).join(" ");
    assert!(r7.contains("No R7 evidence yet"), "{r7}");
    let bios = page(&search, View::Bios, 100, SizeClass::Medium);
    assert!(
        bios.iter().any(|row| row.contains("none confirmed yet")),
        "{bios:?}"
    );
    let empty = Fixture {
        model: Model::new(),
        now: some_time(),
    };
    for (view, want) in [
        (View::Cycle, "No session has started yet"),
        (View::Bios, "No BIOS profile yet"),
    ] {
        let shown = page(&empty, view, 100, SizeClass::Medium).join(" ");
        assert!(shown.contains(want), "{view:?}: {shown}");
    }
}

#[test]
fn the_bios_page_stars_unconfirmed_offsets_and_dates_the_confirmation() {
    let unconfirmed = Fixture::scenario(Scenario::HuntBios);
    for (width, class) in CLASSES {
        let rows = page(&unconfirmed, View::Bios, width, class);
        let shown = rows.join("\n");
        assert!(rows[0].starts_with("BIOS PROFILE"), "{shown}");
        assert!(shown.contains("UNCONFIRMED"), "{shown}");
        assert!(rows.iter().any(|row| row.contains('*')), "{shown}");
        assert!(
            shown.contains("unconfirmed since journal event #"),
            "{shown}"
        );
    }
    let confirmed = Fixture::scenario(Scenario::FirstResult);
    let shown = page(&confirmed, View::Bios, 100, SizeClass::Medium).join("\n");
    assert!(
        shown.contains("confirmed by the passed full cycle"),
        "{shown}"
    );
    assert!(shown.contains("evidence: journal event #"), "{shown}");
    assert!(!shown.contains("ending #"), "{shown}");
}

#[test]
fn the_bios_page_stays_after_the_session_stopped() {
    let fx = Fixture::scenario(Scenario::FirstResult);
    let rows = page(&fx, View::Bios, 100, SizeClass::Medium);
    assert!(rows[0].starts_with("BIOS PROFILE"), "{rows:?}");
    assert!(rows.iter().any(|row| row.starts_with("CCD 0")), "{rows:?}");
}

#[test]
fn the_other_pages_have_no_body_here() {
    let fx = Fixture::scenario(Scenario::Checking);
    for view in [
        View::Main,
        View::Cores,
        View::History,
        View::Log,
        View::Help,
    ] {
        assert!(
            page(&fx, view, 100, SizeClass::Medium).is_empty(),
            "{view:?}"
        );
    }
}

#[test]
fn the_cycle_page_at_the_reference_width_holds_every_value_whole() {
    let fx = Fixture::scenario(Scenario::Checking);
    for class in [SizeClass::Compact, SizeClass::Medium, SizeClass::Wide] {
        let rows = page(&fx, View::Cycle, 154, class);
        assert!(
            rows.iter().all(|row| !row.contains("...")),
            "{class:?}: a value is cut: {rows:?}"
        );
    }
}
