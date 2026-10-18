//! The frames of a journal that is missing, still starting or unreadable.

use jiff::tz::TimeZone;

use super::{
    ALL_SIZES, PROFILES, SIZES, VIEWS, assert_glyphs_safe, draw_journal, main_screen, one_frame,
    some_time, text, words,
};
use crate::frames::testing::screen;
use crate::screen::{Hold, Journal, Screen};

const REASON: &str = "journal line 1: invalid character 'g'";
const RETRY: &str =
    "I'll retry when the journal changes. Tuning itself is not affected by this screen.";

fn problem(text: &str) -> std::io::Error {
    std::io::Error::other(text.to_owned())
}

#[test]
fn a_journal_that_is_missing_starting_or_unreadable_stays_in_bounds_and_never_holds() {
    let error = problem(REASON);
    for journal in [
        Journal::Missing,
        Journal::Starting,
        Journal::Problem(&error),
    ] {
        for (width, height) in ALL_SIZES {
            for view in VIEWS {
                for colours in PROFILES {
                    for keys in [true, false] {
                        let sc = screen(view, width, height, colours, keys);
                        let (buf, drawn) = draw_journal(&journal, &sc, some_time(), &TimeZone::UTC);
                        assert_glyphs_safe(
                            &buf,
                            colours,
                            &format!("{journal:?} {width}x{height} {colours:?}"),
                        );
                        assert_eq!(drawn.hold, Hold::Never, "{journal:?} {width}x{height}");
                    }
                }
            }
        }
    }
}

#[test]
fn a_missing_journal_says_no_session_yet_and_an_unreadable_one_says_why() {
    let error = problem("read journal: permission denied");
    for (width, height) in SIZES {
        let sc = main_screen(width, height);
        let (missing, _) = draw_journal(&Journal::Missing, &sc, some_time(), &TimeZone::UTC);
        let missing = words(&missing).to_lowercase();
        assert!(
            missing.contains("no session yet"),
            "{width}x{height}: {missing}"
        );
        assert!(!missing.contains("can't read journal"), "{missing}");
        let (unreadable, _) =
            draw_journal(&Journal::Problem(&error), &sc, some_time(), &TimeZone::UTC);
        let unreadable = words(&unreadable);
        assert!(
            unreadable.contains("can't read journal: read journal: permission denied"),
            "{width}x{height}: {unreadable}"
        );
        assert!(!unreadable.to_lowercase().contains("no session yet"));
    }
}

#[test]
fn a_problem_and_a_one_frame_problem_differ_only_by_the_retry_promise() {
    let error = problem("read journal: permission denied");
    for (width, height) in SIZES {
        let (live, live_drawn) = draw_journal(
            &Journal::Problem(&error),
            &Screen {
                keys: false,
                ..main_screen(width, height)
            },
            some_time(),
            &TimeZone::UTC,
        );
        let (once, once_drawn) = draw_journal(
            &Journal::Problem(&error),
            &one_frame(width, height),
            some_time(),
            &TimeZone::UTC,
        );
        let live_words = words(&live);
        assert!(live_words.contains(RETRY), "{width}x{height}: {live_words}");
        let once_words = words(&once);
        assert!(!once_words.contains("retry"), "{once_words}");
        assert_eq!(
            live_words
                .replace(RETRY, "")
                .split_whitespace()
                .collect::<Vec<_>>(),
            once_words.split_whitespace().collect::<Vec<_>>(),
            "{width}x{height}: the one-frame problem is the live one less its promise"
        );
        assert_eq!(live_drawn, once_drawn);
    }
}

#[test]
fn an_unreadable_journal_is_not_a_missing_one_live_and_in_one_frame() {
    let error = problem(REASON);
    for sc in [main_screen(120, 10), one_frame(120, 10)] {
        let (buf, _) = draw_journal(&Journal::Problem(&error), &sc, some_time(), &TimeZone::UTC);
        let said = words(&buf);
        assert!(
            said.contains(&format!("can't read journal: {REASON}")),
            "one_frame {}: {said}",
            sc.one_frame
        );
        assert!(!said.to_lowercase().contains("no session yet"), "{said}");
        assert_eq!(said.contains("retry"), !sc.one_frame, "{said}");
    }
    let (missing, _) = draw_journal(
        &Journal::Missing,
        &main_screen(120, 10),
        some_time(),
        &TimeZone::UTC,
    );
    let missing = words(&missing).to_lowercase();
    assert!(missing.contains("no session yet"), "{missing}");
    assert!(!missing.contains("can't read journal"), "{missing}");
}

#[test]
fn the_starting_screen_says_so_and_names_no_earlier_run() {
    for (width, height) in SIZES {
        let (buf, drawn) = draw_journal(
            &Journal::Starting,
            &main_screen(width, height),
            some_time(),
            &TimeZone::UTC,
        );
        let shown = text(&buf);
        assert!(shown.contains("STARTING"), "{width}x{height}:\n{shown}");
        assert!(shown.contains("starting"), "{width}x{height}:\n{shown}");
        for stale in ["NO SESSION YET", "STOPPED", "no session yet", "session 0h"] {
            assert!(
                !shown.contains(stale),
                "{width}x{height} shows {stale:?}:\n{shown}"
            );
        }
        assert_eq!(drawn.hold, Hold::Never);
    }
}
