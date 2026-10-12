//! insta snapshots of whole frames, every one drawn by `render` through the bounds check: the main view of each
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

fn check(name: &str, body: &str) {
    insta::with_settings!({ snapshot_suffix => "", prepend_module_to_snapshot => false }, {
        insta::assert_snapshot!(name, body);
    });
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
            for glyph in ['▄', '·', '█'] {
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

fn main_views(scenario: Scenario) {
    let (model, now) = scenario_model(scenario);
    let journal = ready(&model);
    for size in MAIN_SIZES {
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
    ($($test:ident: $scenario:ident),* $(,)?) => {
        const LISTED: &[Scenario] = &[$(Scenario::$scenario),*];

        $(
            #[test]
            fn $test() {
                main_views(Scenario::$scenario);
            }
        )*
    };
}

main_view_tests! {
    search: Search,
    confirm: Confirm,
    checking: Checking,
    hunt: Hunt,
    member_probe: MemberProbe,
    round: Round,
    confirmation: Confirmation,
    concluded: Concluded,
    unconfirmed: Unconfirmed,
    hunt_bios: HuntBios,
    idle: Idle,
    crashed: Crashed,
    recovering: Recovering,
    combination: Combination,
    between: Between,
    deadend: Deadend,
    first_result: FirstResult,
    stopped: Stopped,
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
