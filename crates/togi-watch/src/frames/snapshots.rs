//! Embedded snapshots of whole frames, every one drawn by `render` through the bounds check: the main view of each
//! simulated scenario, the colour profiles, the journal states, the help and journal views and one frame.
//!
//! A text snapshot is the frame's rows; a styled one adds a row of tone letters under each row that has styling.

use std::ffi::OsStr;

use jiff::Timestamp;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use togi_render::OutputFacts;
use togi_watchtest::Scenario;

use super::colours::colours_for;
use super::testing::{
    FIXED_NOW, assert_glyphs_drawable, ready, render_checked, rows_of, scenario_model, screen,
    styled_snapshot, text_snapshot,
};
use crate::screen::{Colours, Journal, Screen, View};

const MAIN_SIZES: [(u16, u16); 3] = [(240, 67), (160, 45), (120, 33)];
const VIEW_SIZES: [(u16, u16); 3] = [(80, 24), (160, 45), (240, 67)];
const WIDE: (u16, u16) = (240, 67);
const WIDE_ONLY: [(u16, u16); 1] = [WIDE];

macro_rules! expected {
    ($name:expr; $($snapshot:literal),* $(,)?) => {
        match $name {
            $($snapshot => include_str!(concat!("snapshots/", $snapshot, ".snap")),)*
            name => panic!("no embedded frame snapshot for {name}"),
        }
    };
}

#[track_caller]
fn check(name: &str, body: &str) {
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

fn first_row_with<'a>(rows: &'a [String], word: &str) -> &'a str {
    rows.iter()
        .map(String::as_str)
        .find(|row| row.contains(word))
        .unwrap_or_else(|| panic!("no row has {word:?}"))
}

fn assert_nothing_runs(scenario: Scenario, rows: &[String]) {
    const STATES: [&str; 8] = [
        "SEARCH", "CONFIRM", "HAS ROOM", "AT LIMIT", "PARKED", "MEMBER", "PROBE", "SUSPECT",
    ];
    for row in rows.iter().skip(3) {
        let table = STATES.iter().any(|state| row.contains(state))
            || row.contains("search step")
            || row.contains("confirm:");
        assert!(
            !(table && row.contains('►')),
            "{}: {row:?} marks a core or turn as running",
            scenario.name()
        );
        assert!(
            !row.contains("► trial"),
            "{}: {row:?} marks a part as running",
            scenario.name()
        );
        assert!(
            !row.contains("under load") || row.contains("not under load"),
            "{}: {row:?} puts cores under load",
            scenario.name()
        );
    }
}

fn shapes_without_colour(scenario: Scenario, buf: &Buffer) {
    let rows = rows_of(buf);
    if matches!(
        scenario,
        Scenario::Crashed
            | Scenario::Recovering
            | Scenario::Deadend
            | Scenario::Stopped
            | Scenario::FirstResult
            | Scenario::Combination
            | Scenario::Between
    ) {
        assert_nothing_runs(scenario, &rows);
    }
    let bar = |glyph: char| {
        rows.iter()
            .map(|row| row.trim_start())
            .find(|row| row.starts_with(glyph))
            .unwrap_or_else(|| panic!("no bar row starts with {glyph:?}"))
    };
    match scenario {
        Scenario::Checking => {
            let elapsed = bar('█');
            assert!(elapsed.trim_start_matches('█').starts_with('░'));
            assert!(bar('▀').starts_with("▀▀▀▀"));
            let core = first_row_with(&rows, "HAS ROOM");
            for glyph in ['▄', '·', '▬'] {
                assert!(core.contains(glyph), "{glyph:?} missing from {core:?}");
            }
        }
        Scenario::MemberProbe => {
            let probe = first_row_with(&rows, "PROBE");
            assert!(probe.contains('▄') && probe.contains('▀'), "{probe:?}");
            assert!(first_row_with(&rows, "PARKED").contains('·'));
        }
        Scenario::Round => {
            assert!(first_row_with(&rows, "AT LIMIT").contains('░'));
        }
        Scenario::Idle => {
            let quiet = bar('░');
            assert!(quiet.trim_start_matches('░').starts_with(' '));
            assert!(!rows.iter().any(|row| row.trim_start().starts_with('█')));
        }
        Scenario::Deadend => {
            let stage = &rows[2];
            assert!(!stage.contains('►') && stage.contains('○'), "{stage:?}");
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
        let buf = text(
            &name,
            &journal,
            now,
            &screen_of(View::Main, size, Colours::None, 0, false),
        );
        if size == WIDE {
            shapes_without_colour(scenario, &buf);
        }
    }
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
fn every_scenario_fits_the_frame_at_every_main_size() {
    for scenario in Scenario::ALL {
        let (model, now) = scenario_model(scenario);
        let journal = ready(&model);
        for size in MAIN_SIZES {
            let sc = screen_of(View::Main, size, Colours::None, 0, false);
            let (buf, _) = render_checked(&journal, &sc, now);
            assert_glyphs_drawable(
                &buf,
                &format!("main-{}-{}x{}", scenario.name(), size.0, size.1),
            );
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
    for (profile, colours) in [("full", full), ("console", console), ("none", none)] {
        let name = format!("main-{}-{}x{}-{profile}", scenario.name(), WIDE.0, WIDE.1);
        let buf = styled(
            &name,
            &journal,
            now,
            &screen_of(View::Main, WIDE, colours, 0, false),
        );
        assert!(has_no_attribute(&buf), "{name} draws an attribute");
        assert_eq!(has_no_styling(&buf), colours == Colours::None, "{name}");
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
            !said.contains("close view") && !said.contains("help"),
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
