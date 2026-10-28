//! Embedded snapshots of whole frames, every one drawn by `render` through the bounds check: the main view of each
//! hand-built [`Staged`] frame, the colour profiles, the journal states, the help and journal views and one frame.
//!
//! A text snapshot is the frame's rows; a styled one adds a row of tone letters under each row that has styling.

use std::ffi::OsStr;

use jiff::Timestamp;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use togi_render::OutputFacts;

use super::colours::colours_for;
use super::hand::Staged;
use super::testing::{
    FIXED_NOW, assert_glyphs_drawable, render_checked, rows_of, screen, styled_snapshot,
    text_snapshot,
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
        "help-120x33-rich",
        "help-160x45",
        "help-160x45-rich",
        "help-240x67",
        "help-240x67-full",
        "help-240x67-rich",
        "help-80x24",
        "help-end-160x45",
        "help-end-240x67",
        "help-end-240x67-full",
        "help-end-80x24",
        "log-160x45",
        "log-240x67",
        "log-240x67-full",
        "log-80x24",
        "log-top-160x45",
        "main-between-240x67",
        "main-crashed-240x67",
        "main-day-120x33",
        "main-day-160x45",
        "main-day-240x67",
        "main-deadend-240x67",
        "main-idle-120x33",
        "main-idle-160x45",
        "main-idle-240x67",
        "main-idle-240x67-console",
        "main-idle-240x67-full",
        "main-idle-240x67-none",
        "main-mapping-120x33",
        "main-mapping-120x33-rich",
        "main-mapping-160x45",
        "main-mapping-160x45-rich",
        "main-mapping-240x67",
        "main-mapping-240x67-console",
        "main-mapping-240x67-full",
        "main-mapping-240x67-none",
        "main-mapping-240x67-rich",
        "main-qualifying-120x33-rich",
        "main-qualifying-160x45-rich",
        "main-qualifying-240x67",
        "main-qualifying-240x67-console",
        "main-qualifying-240x67-full",
        "main-qualifying-240x67-none",
        "main-qualifying-240x67-rich",
        "main-recovering-120x33",
        "main-recovering-160x45",
        "main-recovering-240x67",
        "main-reproducing-240x67",
        "main-stopped-240x67",
        "missing-120x33",
        "missing-160x45",
        "missing-240x67",
        "one-frame-mapping-120x33",
        "one-frame-mapping-240x67",
        "problem-120x33",
        "problem-160x45",
        "problem-240x67",
        "problem-once-120x33",
        "problem-once-160x45",
        "problem-once-240x67",
        "starting-120x33",
        "starting-160x45",
        "starting-240x67",
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

/// What a frame's shapes say without colour: the glyphs that carry meaning on their own.
fn shapes_without_colour(staged: Staged, buf: &Buffer) {
    let rows = rows_of(buf);
    let bar = |glyph: char| {
        rows.iter()
            .map(|row| row.trim_start())
            .find(|row| row.starts_with(glyph))
            .unwrap_or_else(|| panic!("no bar row starts with {glyph:?}"))
    };
    match staged {
        Staged::Mapping | Staged::Qualifying | Staged::Reproducing | Staged::Day => {
            let elapsed = bar('█');
            assert!(elapsed.trim_start_matches('█').starts_with('░'));
            let core = first_row_with(&rows, "LOAD");
            for glyph in ['▄', '·'] {
                assert!(
                    core.contains(glyph) || staged != Staged::Mapping,
                    "{glyph:?} missing from {core:?}"
                );
            }
        }
        Staged::Idle => {
            let quiet = bar('░');
            assert!(quiet.trim_start_matches('░').starts_with(' '));
            assert!(!rows.iter().any(|row| row.trim_start().starts_with('█')));
        }
        Staged::Crashed | Staged::Recovering | Staged::Between | Staged::Stopped => {
            for row in rows.iter().skip(3) {
                assert!(
                    !row.contains('►'),
                    "{}: {row:?} marks a core as running",
                    staged.name()
                );
            }
        }
        Staged::Deadend => {
            let stage = &rows[2];
            assert!(!stage.contains('►') && stage.contains('○'), "{stage:?}");
        }
    }
}

fn main_views(staged: Staged, sizes: &[(u16, u16)]) {
    let hand = staged.hand();
    let journal = hand.journal();
    for &size in sizes {
        let name = format!("main-{}-{}x{}", staged.name(), size.0, size.1);
        let buf = text(
            &name,
            &journal,
            hand.now,
            &screen_of(View::Main, size, Colours::None, 0, false),
        );
        if size == WIDE {
            shapes_without_colour(staged, &buf);
        }
    }
}

// A frame keeps a snapshot at every main size only when it draws a size-dependent panel no other frame draws; the
// rest keep the wide one, and the bounds matrix in `frame_tests/bounds.rs` still draws them all at every size.
// mapping: the CCD tables beside each other and stacked, the night's rows. day: the day's rows with no night.
// idle: the planned end. recovering: the band after a crash. qualifying and reproducing: the night's other rows.
// The wide-only frames draw panels a kept frame already draws: crashed, between, deadend and stopped a band.
const MAIN_CASES: &[(Staged, &[(u16, u16)])] = &[
    (Staged::Mapping, &MAIN_SIZES),
    (Staged::Qualifying, &WIDE_ONLY),
    (Staged::Reproducing, &WIDE_ONLY),
    (Staged::Day, &MAIN_SIZES),
    (Staged::Idle, &MAIN_SIZES),
    (Staged::Crashed, &WIDE_ONLY),
    (Staged::Recovering, &MAIN_SIZES),
    (Staged::Between, &WIDE_ONLY),
    (Staged::Deadend, &WIDE_ONLY),
    (Staged::Stopped, &WIDE_ONLY),
];

macro_rules! main_view_tests {
    ($($test:ident: $staged:ident),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                let sizes = MAIN_CASES
                    .iter()
                    .find(|(staged, _)| *staged == Staged::$staged)
                    .map(|&(_, sizes)| sizes)
                    .expect("the frame is listed in MAIN_CASES");
                main_views(Staged::$staged, sizes);
            }
        )*
    };
}

main_view_tests! {
    mapping: Mapping,
    qualifying: Qualifying,
    reproducing: Reproducing,
    day: Day,
    idle: Idle,
    crashed: Crashed,
    recovering: Recovering,
    between: Between,
    deadend: Deadend,
    stopped: Stopped,
}

#[test]
fn every_staged_frame_has_a_main_view_snapshot() {
    assert_eq!(MAIN_CASES.len(), Staged::ALL.len());
    for staged in Staged::ALL {
        assert!(
            MAIN_CASES.iter().any(|(listed, _)| *listed == staged),
            "{} has no snapshot",
            staged.name()
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
    assert_eq!(colours_for(terminal(), xterm, None), Colours::Full);
    assert_eq!(colours_for(terminal(), linux, None), Colours::Console);
    assert_eq!(colours_for(no_color, xterm, None), Colours::None);
    assert_eq!(colours_for(journald, xterm, None), Colours::None);
    assert_eq!(colours_for(no_color, linux, None), Colours::None);
    assert_eq!(colours_for(journald, linux, None), Colours::None);
}

fn profiles(staged: Staged) {
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
    let hand = staged.hand();
    let journal = hand.journal();
    let now = hand.now;
    let full = colours_for(terminal(), xterm, None);
    let console = colours_for(terminal(), linux, None);
    let none = colours_for(no_color, xterm, None);
    assert_eq!(none, colours_for(journald, xterm, None));
    for (profile, colours) in [("full", full), ("console", console), ("none", none)] {
        let name = format!("main-{}-{}x{}-{profile}", staged.name(), WIDE.0, WIDE.1);
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
fn mapping_in_each_colour_profile() {
    profiles(Staged::Mapping);
}

#[test]
fn qualifying_in_each_colour_profile() {
    profiles(Staged::Qualifying);
}

#[test]
fn idle_in_each_colour_profile() {
    profiles(Staged::Idle);
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
    let hand = Staged::Mapping.hand();
    let journal = hand.journal();
    let now = hand.now;
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
    let hand = Staged::Stopped.hand();
    let journal = hand.journal();
    let now = hand.now;
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
    let hand = Staged::Mapping.hand();
    let journal = hand.journal();
    let now = hand.now;
    for size in [WIDE, (120, 33)] {
        let (width, height) = size;
        let once = screen_of(View::Main, size, Colours::None, 0, true);
        assert!(!once.keys && once.one_frame);
        let buf = text(
            &format!("one-frame-mapping-{width}x{height}"),
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

fn rich_frames(mut visit: impl FnMut(&str, &Buffer)) {
    for (staged, view, prefix) in [
        (Staged::Mapping, View::Main, "main-mapping"),
        (Staged::Qualifying, View::Main, "main-qualifying"),
        (Staged::Mapping, View::Help, "help"),
    ] {
        let hand = staged.hand();
        for size in MAIN_SIZES {
            let sc = screen_of(view, size, Colours::Rich, 0, false);
            let (buf, _) = render_checked(&hand.journal(), &sc, hand.now);
            visit(&format!("{prefix}-{}x{}-rich", size.0, size.1), &buf);
        }
    }
}

#[test]
fn rich_frames_preserve_their_glyphs_and_have_no_controls_or_attributes() {
    rich_frames(|name, buf| {
        let body = styled_snapshot(buf, Colours::Rich);
        assert!(!body.chars().any(|c| c.is_control() && c != '\n'), "{name}");
        assert!(has_no_attribute(buf), "{name}");
        assert!(body.contains('▆'), "{name}");
        assert!(!body.contains(r"\u2586"), "{name}");
        check(name, &body);
    });
}

/// Regenerate fixtures explicitly, outside the ordinary in-memory tests:
/// `TOGI_SNAPSHOTS="${TMPDIR:-/tmp}/togi-snaps" cargo test -p togi-watch real_fs_generate_snapshots -- --ignored`.
/// Review and copy the emitted `.snap` files into this module's `snapshots/` directory.
#[test]
#[ignore = "writes regenerated snapshot fixtures to an explicitly supplied temporary directory"]
fn real_fs_generate_snapshots() {
    let directory = std::env::var_os("TOGI_SNAPSHOTS").expect("set TOGI_SNAPSHOTS");
    let directory = std::path::Path::new(&directory);
    assert!(
        directory.starts_with(std::env::temp_dir()),
        "use a temporary directory"
    );
    std::fs::create_dir_all(directory).unwrap();
    let emit = |name: &str, body: String| {
        std::fs::write(
            directory.join(format!("{name}.snap")),
            format!("---\nsource: crates/togi-watch/src/frames/snapshots.rs\nexpression: body\n---\n{body}"),
        )
        .unwrap();
    };
    rich_frames(|name, buf| emit(name, styled_snapshot(buf, Colours::Rich)));
    for &(staged, sizes) in MAIN_CASES {
        let hand = staged.hand();
        for &size in sizes {
            let sc = screen_of(View::Main, size, Colours::None, 0, false);
            let (buf, _) = render_checked(&hand.journal(), &sc, hand.now);
            emit(
                &format!("main-{}-{}x{}", staged.name(), size.0, size.1),
                text_snapshot(&buf),
            );
        }
    }
    for staged in [Staged::Mapping, Staged::Qualifying, Staged::Idle] {
        let hand = staged.hand();
        for (profile, colours) in [
            ("full", Colours::Full),
            ("console", Colours::Console),
            ("none", Colours::None),
        ] {
            let sc = screen_of(View::Main, WIDE, colours, 0, false);
            let (buf, _) = render_checked(&hand.journal(), &sc, hand.now);
            emit(
                &format!("main-{}-{}x{}-{profile}", staged.name(), WIDE.0, WIDE.1),
                styled_snapshot(&buf, colours),
            );
        }
    }
    let hand = Staged::Mapping.hand();
    for size in VIEW_SIZES {
        for (prefix, scroll) in [("help", 0), ("help-end", usize::MAX)] {
            let sc = screen_of(View::Help, size, Colours::None, scroll, false);
            let (buf, _) = render_checked(&hand.journal(), &sc, hand.now);
            emit(
                &format!("{prefix}-{}x{}", size.0, size.1),
                text_snapshot(&buf),
            );
            if size == WIDE {
                let sc = screen_of(View::Help, size, Colours::Full, scroll, false);
                let (buf, _) = render_checked(&hand.journal(), &sc, hand.now);
                emit(
                    &format!("{prefix}-{}x{}-full", size.0, size.1),
                    styled_snapshot(&buf, Colours::Full),
                );
            }
        }
    }
    for size in [WIDE, (120, 33)] {
        let sc = screen_of(View::Main, size, Colours::None, 0, true);
        let (buf, _) = render_checked(&hand.journal(), &sc, hand.now);
        emit(
            &format!("one-frame-mapping-{}x{}", size.0, size.1),
            text_snapshot(&buf),
        );
    }
    let stopped = Staged::Stopped.hand();
    for size in VIEW_SIZES {
        let sc = screen_of(View::Log, size, Colours::None, usize::MAX, false);
        let (buf, _) = render_checked(&stopped.journal(), &sc, stopped.now);
        emit(&format!("log-{}x{}", size.0, size.1), text_snapshot(&buf));
    }
    let sc = screen_of(View::Log, (160, 45), Colours::None, 0, false);
    let (buf, _) = render_checked(&stopped.journal(), &sc, stopped.now);
    emit("log-top-160x45", text_snapshot(&buf));
    let sc = screen_of(View::Log, WIDE, Colours::Full, usize::MAX, false);
    let (buf, _) = render_checked(&stopped.journal(), &sc, stopped.now);
    emit(
        &format!("log-{}x{}-full", WIDE.0, WIDE.1),
        styled_snapshot(&buf, Colours::Full),
    );
}
