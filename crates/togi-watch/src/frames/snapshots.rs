//! Embedded snapshots of whole frames, every one drawn by `render` through the bounds check: the main view of each
//! simulated scenario, the colour profiles, the journal states, the help and journal views and one frame.
//!
//! A text snapshot is the frame's rows; a styled one adds a row of tone letters under each row that has styling.

use std::ffi::OsStr;
use std::path::Path;

use jiff::Timestamp;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use togi_render::OutputFacts;
use togi_watchtest::Scenario;

use super::colours::colours_for;
use super::measure::Main;
use super::testing::{
    FIXED_NOW, assert_glyphs_drawable, ready, render_checked, rows_of, scenario_model, screen,
    styled_snapshot, text_snapshot,
};
use crate::screen::{Colours, Journal, Screen, View};

const MAIN_SIZES: [(u16, u16); 3] = [(240, 67), (160, 45), (120, 33)];
const VIEW_SIZES: [(u16, u16); 3] = [(80, 24), (160, 45), (240, 67)];
const WIDE: (u16, u16) = (240, 67);
const WIDE_ONLY: [(u16, u16); 1] = [WIDE];
const REFERENCE: (u16, u16) = (160, 45);
/// Both sides of the reference size's boundary, the numeric table's smallest size and one cell under it.
const COMPACT_SIZES: [(u16, u16); 4] = [(159, 45), (160, 44), (80, 30), (79, 29)];

macro_rules! expected {
    ($name:expr; $($snapshot:literal),* $(,)?) => {
        match $name {
            $($snapshot => include_str!(concat!("snapshots/", $snapshot, ".snap")),)*
            name => panic!("no embedded frame snapshot for {name}"),
        }
    };
}

/// Set to rewrite every compared snapshot from the frame drawn now instead of comparing; a name new to the list
/// below needs its (empty) file to exist first, because the fixtures are compiled in.
const BLESS: &str = "TOGI_BLESS_FRAMES";

fn write_snapshot(name: &str, body: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/frames/snapshots")
        .join(format!("{name}.snap"));
    let header = "---\nsource: crates/togi-watch/src/frames/snapshots.rs\nexpression: body\n---\n";
    std::fs::write(&path, format!("{header}{body}"))
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

#[track_caller]
fn check(name: &str, body: &str) {
    if std::env::var_os(BLESS).is_some() {
        write_snapshot(name, body);
        return;
    }
    // Compile the committed fixtures into the test: comparisons never invoke insta's disk/config machinery.
    let snapshot = expected! { name;
        "main-between-240x67",
        "main-stopped-240x67",
        "main-unconfirmed-240x67",
        "one-frame-checking-120x33",
        "one-frame-checking-240x67",
        "main-checking-120x33",
        "main-checking-160x45",
        "main-checking-240x67-console",
        "main-checking-240x67-full",
        "main-checking-240x67-none",
        "main-checking-240x67",
        "main-concluded-240x67",
        "main-confirmation-240x67",
        "main-deadend-240x67",
        "main-first-result-240x67",
        "main-hunt-120x33",
        "main-hunt-160x45",
        "main-hunt-240x67",
        "main-hunt-bios-120x33",
        "main-hunt-bios-160x45",
        "main-hunt-bios-240x67",
        "main-idle-120x33",
        "main-idle-160x45",
        "main-idle-240x67-console",
        "main-idle-240x67-full",
        "main-idle-240x67-none",
        "main-idle-240x67",
        "help-160x45",
        "help-240x67-full",
        "help-240x67",
        "help-80x24",
        "help-end-240x67-full",
        "help-end-240x67",
        "help-end-160x45",
        "help-end-80x24",
        "main-member-probe-240x67-console",
        "main-member-probe-240x67-full",
        "main-member-probe-240x67-none",
        "main-member-probe-240x67",
        "main-recovering-120x33",
        "main-recovering-160x45",
        "main-recovering-240x67",
        "main-round-120x33",
        "main-round-160x45",
        "main-round-240x67",
        "main-search-120x33",
        "main-search-160x45",
        "main-search-240x67",
        "missing-120x33",
        "missing-160x45",
        "missing-240x67",
        "problem-120x33",
        "problem-160x45",
        "problem-240x67",
        "problem-once-120x33",
        "problem-once-160x45",
        "problem-once-240x67",
        "starting-120x33",
        "starting-160x45",
        "starting-240x67",
        "main-combination-120x33",
        "main-combination-160x45",
        "main-combination-240x67",
        "main-confirm-240x67",
        "main-crashed-240x67",
        "main-member-probe-120x33",
        "main-member-probe-160x45",
        "log-160x45",
        "log-240x67-full",
        "log-240x67",
        "log-80x24",
        "log-top-160x45",
        "main-checking-159x45",
        "main-checking-160x44",
        "main-checking-80x30",
        "main-checking-79x29",
        "main-stopped-80x30",
        "main-stopped-79x29",
        "main-checking-160x45-full",
        "main-checking-160x45-console",
        "main-checking-160x45-none",
        "main-member-probe-160x45-full",
        "main-member-probe-160x45-console",
        "main-member-probe-160x45-none",
        "main-idle-160x45-full",
        "main-idle-160x45-console",
        "main-idle-160x45-none",
        "page-cores-search-160x45",
        "page-cycle-search-160x45",
        "page-r7-search-160x45",
        "page-hunts-search-160x45",
        "page-bios-search-160x45",
        "page-history-search-160x45",
        "page-cores-checking-160x45",
        "page-cores-stopped-160x45",
        "page-cycle-checking-160x45",
        "page-cycle-confirmation-160x45",
        "page-r7-checking-160x45",
        "page-hunts-hunt-160x45",
        "page-hunts-member-probe-160x45",
        "page-bios-hunt-bios-160x45",
        "page-bios-unconfirmed-160x45",
        "page-history-stopped-160x45",
    };
    let (_, expected) = snapshot
        .strip_prefix("---\n")
        .and_then(|snapshot| snapshot.split_once("\n---\n"))
        .expect("a committed snapshot has an insta metadata header");
    // The row assertion shows the first changed row rather than an escaped whole-frame string.
    for (row, (actual, wanted)) in body.lines().zip(expected.lines()).enumerate() {
        assert_eq!(actual, wanted, "{name}, row {}", row + 1);
    }
    assert_eq!(
        body, expected,
        "{name}: frame length or trailing newline differs"
    );
}

fn screen_of(
    view: View,
    (width, height): (u16, u16),
    colours: Colours,
    scroll: usize,
    one_frame: bool,
) -> Screen {
    Screen {
        scroll,
        one_frame,
        ..screen(view, width, height, colours, !one_frame)
    }
}

fn text(name: &str, journal: &Journal<'_>, now: Timestamp, sc: &Screen) -> Buffer {
    let (buf, _) = render_checked(journal, sc, now);
    assert_glyphs_drawable(&buf, name);
    check(name, &text_snapshot(&buf));
    buf
}

fn styled(name: &str, journal: &Journal<'_>, now: Timestamp, sc: &Screen) -> Buffer {
    let (buf, _) = render_checked(journal, sc, now);
    assert_glyphs_drawable(&buf, name);
    check(name, &styled_snapshot(&buf, sc.colours));
    buf
}

fn has_no_attribute(buf: &Buffer) -> bool {
    buf.content().iter().all(|cell| cell.modifier.is_empty())
}

fn has_no_styling(buf: &Buffer) -> bool {
    buf.content()
        .iter()
        .all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset && cell.modifier.is_empty())
}

fn terminal() -> OutputFacts {
    OutputFacts {
        is_terminal: true,
        ..OutputFacts::default()
    }
}

fn shapes_without_colour(scenario: Scenario, buf: &Buffer) {
    let rows = rows_of(buf);
    let stage = &rows[1];
    if matches!(
        scenario,
        Scenario::Crashed | Scenario::Recovering | Scenario::Deadend | Scenario::Stopped
    ) {
        assert!(
            !stage.contains('►'),
            "{}: {stage:?} marks a step as running",
            scenario.name()
        );
    }
    let main = Main::new(&screen_of(View::Main, REFERENCE, Colours::None, 0, false));
    let trial = &rows[usize::from(main.now.y) + 1];
    let bar_cells = trial.chars().filter(|c| matches!(c, '█' | '░')).count();
    let gauges = &rows[usize::from(main.gauges[0].y)..usize::from(main.gauges[0].bottom())];
    let marked = |from: u16| {
        gauges.iter().any(|row| {
            row.chars()
                .skip(usize::from(from))
                .take(usize::from(main.gauges[0].width))
                .any(|c| c == '█')
        })
    };
    match scenario {
        Scenario::Checking => {
            assert_eq!(bar_cells, 24, "{trial:?}: the trial bar is 24 cells");
            assert!(
                marked(main.gauges[0].x) && marked(main.gauges[1].x),
                "both gauges draw tune bars"
            );
        }
        Scenario::MemberProbe => assert!(marked(main.gauges[0].x) || marked(main.gauges[1].x)),
        Scenario::Idle => {
            assert!(
                !trial.contains('█'),
                "{trial:?}: an idle trial's bar has no elapsed fill"
            );
            assert!(trial.contains("planned"), "{trial:?}");
        }
        Scenario::Between => {
            for row in &rows {
                let words: Vec<&str> = row.split_whitespace().collect();
                for pair in words.windows(2) {
                    let counts = pair[0].split_once('/');
                    if let (Some((done, of)), "passed") = (counts, pair[1])
                        && let (Ok(done), Ok(of)) = (done.parse::<u32>(), of.parse::<u32>())
                    {
                        assert!(done <= of, "{row:?} counts more passes than it needs");
                    }
                }
            }
        }
        _ => {}
    }
}

fn main_views(scenario: Scenario, sizes: &[(u16, u16)]) {
    let (model, now) = scenario_model(scenario);
    let journal = ready(&model);
    for &size in sizes {
        let name = format!("main-{}-{}x{}", scenario.name(), size.0, size.1);
        text(
            &name,
            &journal,
            now,
            &screen_of(View::Main, size, Colours::None, 0, false),
        );
    }
    let sc = screen_of(View::Main, REFERENCE, Colours::None, 0, false);
    let (buf, _) = render_checked(&journal, &sc, now);
    shapes_without_colour(scenario, &buf);
}

macro_rules! main_view_tests {
    ($($test:ident: $scenario:ident in $sizes:ident),* $(,)?) => {
        const LISTED: &[Scenario] = &[$(Scenario::$scenario),*];

        $(
            #[test]
            fn $test() {
                main_views(Scenario::$scenario, &$sizes);
            }
        )*
    };
}

// A cut keeps a snapshot at every main size only when it draws a size-dependent panel no other kept cut draws;
// the rest keep the wide one, and `every_scenario_fits_the_frame_at_every_main_size` still draws them all.
// search: the turns table. checking: the cycle and partial tables. hunt: the hunt plan and its parts. member_probe:
// the probe rows. round: the deepening checks. idle: the planned end. recovering: the band after a crash.
// combination: the compact member columns. hunt_bios: the hunt's evidence rows and the unconfirmed profile.
// The wide-only cuts draw panels a kept cut already draws: confirm the turns table (search), confirmation and
// concluded the cycle table (checking), unconfirmed the unconfirmed profile (hunt_bios), and between, stopped,
// first_result, deadend and crashed a band or a plain cycle table.
main_view_tests! {
    search: Search in MAIN_SIZES,
    confirm: Confirm in WIDE_ONLY,
    checking: Checking in MAIN_SIZES,
    hunt: Hunt in MAIN_SIZES,
    member_probe: MemberProbe in MAIN_SIZES,
    round: Round in MAIN_SIZES,
    confirmation: Confirmation in WIDE_ONLY,
    concluded: Concluded in WIDE_ONLY,
    unconfirmed: Unconfirmed in WIDE_ONLY,
    hunt_bios: HuntBios in MAIN_SIZES,
    idle: Idle in MAIN_SIZES,
    crashed: Crashed in WIDE_ONLY,
    recovering: Recovering in MAIN_SIZES,
    combination: Combination in MAIN_SIZES,
    between: Between in WIDE_ONLY,
    deadend: Deadend in WIDE_ONLY,
    first_result: FirstResult in WIDE_ONLY,
    stopped: Stopped in WIDE_ONLY,
}

#[test]
fn every_scenario_has_a_main_view_snapshot() {
    assert_eq!(LISTED.len(), Scenario::ALL.len());
    for scenario in Scenario::ALL {
        assert!(
            LISTED.contains(&scenario),
            "{} has no test",
            scenario.name()
        );
    }
}

#[test]
fn every_scenario_fits_the_frame_on_every_page_at_every_size() {
    for scenario in Scenario::ALL {
        let (model, now) = scenario_model(scenario);
        let journal = ready(&model);
        for size in MAIN_SIZES.into_iter().chain(COMPACT_SIZES) {
            for view in View::ALL {
                let sc = screen_of(view, size, Colours::None, 0, false);
                let (buf, _) = render_checked(&journal, &sc, now);
                assert_glyphs_drawable(
                    &buf,
                    &format!("{}-{}-{}x{}", slug(view), scenario.name(), size.0, size.1),
                );
            }
        }
    }
}

#[test]
fn no_color_and_journald_map_to_one_profile_and_the_console_to_its_own() {
    let xterm = Some(OsStr::new("xterm-256color"));
    let linux = Some(OsStr::new("linux"));
    let no_color = OutputFacts {
        no_color: true,
        ..terminal()
    };
    let journald = OutputFacts {
        journald: true,
        ..terminal()
    };
    assert_eq!(colours_for(terminal(), xterm), Colours::Full);
    assert_eq!(colours_for(terminal(), linux), Colours::Console);
    assert_eq!(colours_for(no_color, xterm), Colours::None);
    assert_eq!(colours_for(journald, xterm), Colours::None);
    assert_eq!(colours_for(no_color, linux), Colours::None);
    assert_eq!(colours_for(journald, linux), Colours::None);
}

fn profiles(scenario: Scenario) {
    let xterm = Some(OsStr::new("xterm-256color"));
    let linux = Some(OsStr::new("linux"));
    let no_color = OutputFacts {
        no_color: true,
        ..terminal()
    };
    let journald = OutputFacts {
        journald: true,
        ..terminal()
    };
    let (model, now) = scenario_model(scenario);
    let journal = ready(&model);
    let full = colours_for(terminal(), xterm);
    let console = colours_for(terminal(), linux);
    let none = colours_for(no_color, xterm);
    assert_eq!(none, colours_for(journald, xterm));
    for size in [REFERENCE, WIDE] {
        for (profile, colours) in [("full", full), ("console", console), ("none", none)] {
            let name = format!("main-{}-{}x{}-{profile}", scenario.name(), size.0, size.1);
            let buf = styled(
                &name,
                &journal,
                now,
                &screen_of(View::Main, size, colours, 0, false),
            );
            assert!(has_no_attribute(&buf), "{name} draws an attribute");
            assert_eq!(has_no_styling(&buf), colours == Colours::None, "{name}");
        }
    }
}

#[test]
fn checking_in_each_colour_profile() {
    profiles(Scenario::Checking);
}

#[test]
fn member_probe_in_each_colour_profile() {
    profiles(Scenario::MemberProbe);
}

#[test]
fn idle_in_each_colour_profile() {
    profiles(Scenario::Idle);
}

#[test]
fn journal_states_at_the_main_sizes() {
    let unreadable = std::io::Error::other("events.jsonl: unexpected end of file");
    for size in MAIN_SIZES {
        let (width, height) = size;
        let live = screen_of(View::Main, size, Colours::None, 0, false);
        let once = screen_of(View::Main, size, Colours::None, 0, true);
        text(
            &format!("missing-{width}x{height}"),
            &Journal::Missing,
            FIXED_NOW,
            &live,
        );
        text(
            &format!("starting-{width}x{height}"),
            &Journal::Starting,
            FIXED_NOW,
            &live,
        );
        text(
            &format!("problem-{width}x{height}"),
            &Journal::Problem(&unreadable),
            FIXED_NOW,
            &live,
        );
        text(
            &format!("problem-once-{width}x{height}"),
            &Journal::Problem(&unreadable),
            FIXED_NOW,
            &once,
        );
    }
}

#[test]
fn help_at_the_view_sizes() {
    let (model, now) = scenario_model(Scenario::Checking);
    let journal = ready(&model);
    for size in VIEW_SIZES {
        let (width, height) = size;
        text(
            &format!("help-{width}x{height}"),
            &journal,
            now,
            &screen_of(View::Help, size, Colours::None, 0, false),
        );
        text(
            &format!("help-end-{width}x{height}"),
            &journal,
            now,
            &screen_of(View::Help, size, Colours::None, usize::MAX, false),
        );
    }
    for (what, scroll) in [("help", 0), ("help-end", usize::MAX)] {
        let name = format!("{what}-{}x{}-full", WIDE.0, WIDE.1);
        let sc = screen_of(View::Help, WIDE, Colours::Full, scroll, false);
        styled(&name, &journal, now, &sc);
    }
}

#[test]
fn the_journal_view_at_the_view_sizes() {
    let (model, now) = scenario_model(Scenario::Stopped);
    let journal = ready(&model);
    for size in VIEW_SIZES {
        let (width, height) = size;
        text(
            &format!("log-{width}x{height}"),
            &journal,
            now,
            &screen_of(View::Log, size, Colours::None, usize::MAX, false),
        );
    }
    text(
        "log-top-160x45",
        &journal,
        now,
        &screen_of(View::Log, (160, 45), Colours::None, 0, false),
    );
    let sc = screen_of(View::Log, WIDE, Colours::Full, usize::MAX, false);
    styled(
        &format!("log-{}x{}-full", WIDE.0, WIDE.1),
        &journal,
        now,
        &sc,
    );
}

#[test]
fn one_frame_prints_the_live_text_without_the_key_footer() {
    let (model, now) = scenario_model(Scenario::Checking);
    let journal = ready(&model);
    for size in [WIDE, (120, 33)] {
        let (width, height) = size;
        let once = screen_of(View::Main, size, Colours::None, 0, true);
        assert!(!once.keys && once.one_frame);
        let buf = text(
            &format!("one-frame-checking-{width}x{height}"),
            &journal,
            now,
            &once,
        );
        let said = rows_of(&buf).join("\n");
        assert!(
            !said.contains("exit dashboard") && !said.contains("help"),
            "a one-frame print shows the key footer:\n{said}"
        );
        let live = Screen {
            keys: false,
            ..screen_of(View::Main, size, Colours::None, 0, false)
        };
        let (live_buf, _) = render_checked(&journal, &live, now);
        assert_eq!(rows_of(&buf), rows_of(&live_buf));
    }
}

#[test]
fn the_numeric_main_at_its_boundary_sizes() {
    main_views(Scenario::Checking, &COMPACT_SIZES);
    main_views(Scenario::Stopped, &[(80, 30), (79, 29)]);
}

fn slug(view: View) -> &'static str {
    match view {
        View::Main => "main",
        View::Cores => "cores",
        View::Cycle => "cycle",
        View::R7 => "r7",
        View::Hunts => "hunts",
        View::Bios => "bios",
        View::History => "history",
        View::Log => "log",
        View::Help => "help",
    }
}

// Each detail page over a session that fills it and over the search, which leaves most of them to their
// explicit empty states.
const PAGES: [(View, Scenario); 16] = [
    (View::Cores, Scenario::Search),
    (View::Cycle, Scenario::Search),
    (View::R7, Scenario::Search),
    (View::Hunts, Scenario::Search),
    (View::Bios, Scenario::Search),
    (View::History, Scenario::Search),
    (View::Cores, Scenario::Checking),
    (View::Cores, Scenario::Stopped),
    (View::Cycle, Scenario::Checking),
    (View::Cycle, Scenario::Confirmation),
    (View::R7, Scenario::Checking),
    (View::Hunts, Scenario::Hunt),
    (View::Hunts, Scenario::MemberProbe),
    (View::Bios, Scenario::HuntBios),
    (View::Bios, Scenario::Unconfirmed),
    (View::History, Scenario::Stopped),
];

#[test]
fn every_detail_page_at_the_reference_size() {
    for (view, scenario) in PAGES {
        let (model, now) = scenario_model(scenario);
        let journal = ready(&model);
        let name = format!(
            "page-{}-{}-{}x{}",
            slug(view),
            scenario.name(),
            REFERENCE.0,
            REFERENCE.1
        );
        text(
            &name,
            &journal,
            now,
            &screen_of(view, REFERENCE, Colours::None, 0, false),
        );
    }
}
