//! Whole frames through [`render`](super::render): each test draws a journal on a screen and pins what the frame
//! shows, how it is bounded and how long it holds.

mod arrivals;
mod bands;
mod bounds;
mod day_question;
mod escapes;
mod holds;
mod live_panels;
mod local_time;
mod panels;
mod policy_states;
mod without_journal;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use ratatui::buffer::Buffer;
use togi_journal::{Event, Payload, Stamp};
use togi_machine::BootId;

use super::testing::{Fixture, assert_glyphs_drawable, draw_journal, main_screen, rows_of};
use crate::screen::{Colours, Screen, View};

pub(super) const SIZES: [(u16, u16); 3] = [(240, 67), (160, 45), (120, 33)];
pub(super) const ALL_SIZES: [(u16, u16); 4] = [(240, 67), (160, 45), (120, 33), (80, 24)];
pub(super) const VIEWS: [View; 3] = [View::Main, View::Help, View::Log];
pub(super) const PROFILES: [Colours; 4] = [
    Colours::Full,
    Colours::Rich,
    Colours::Console,
    Colours::None,
];

/// Fails the test when `buf` shows what `colours` cannot draw: glyphs outside IBM437 on the console profiles, any
/// control or bidi formatting character on [`Colours::Rich`], which draws other Unicode as it is.
#[track_caller]
pub(super) fn assert_glyphs_safe(buf: &Buffer, colours: Colours, what: &str) {
    if colours != Colours::Rich {
        assert_glyphs_drawable(buf, what);
        return;
    }
    for (row, text) in rows_of(buf).iter().enumerate() {
        for c in text.chars() {
            assert!(
                !c.is_control()
                    && !matches!(
                        c,
                        '\u{61c}'
                            | '\u{200e}'
                            | '\u{200f}'
                            | '\u{2028}'..='\u{202e}'
                            | '\u{2066}'..='\u{2069}'
                    ),
                "{what} row {row} draws the control {c:?}: {text:?}"
            );
        }
    }
}

pub(super) fn some_time() -> Timestamp {
    Timestamp::from_second(1_700_000_000).expect("a time")
}

pub(super) fn later(time: Timestamp, secs: i64) -> Timestamp {
    time.checked_add(SignedDuration::from_secs(secs))
        .expect("a time")
}

pub(super) fn one_frame(width: u16, height: u16) -> Screen {
    Screen {
        one_frame: true,
        keys: false,
        ..main_screen(width, height)
    }
}

pub(super) fn text(buf: &Buffer) -> String {
    rows_of(buf)
        .iter()
        .map(|row| row.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn words(buf: &Buffer) -> String {
    squash(&text(buf).replace('▌', ""))
}

pub(super) fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn clock(time: Timestamp, zone: &TimeZone, format: &str) -> String {
    time.to_zoned(zone.clone()).strftime(format).to_string()
}

/// A journal of a session on two cores with its configuration loaded, then `payloads`, a second apart.
pub(in crate::frames) fn journal(payloads: Vec<Payload>) -> Vec<Event> {
    let mut events: Vec<Event> = Vec::new();
    let start = [
        crate::tests::kit::session_start(&[(0, 0), (1, 0)]),
        crate::tests::kit::config_loaded(),
    ];
    for (index, payload) in start.into_iter().chain(payloads).enumerate() {
        if events.is_empty() {
            debug_assert_eq!(index, 0);
            events.push(Event::new(
                crate::tests::kit::seq(1),
                Stamp {
                    time: some_time(),
                    mono: None,
                },
                BootId::from("boot-1".to_owned()),
                Vec::new(),
                payload,
            ));
        } else {
            append(&mut events, payload, 1, "boot-1");
        }
    }
    events
}

pub(super) fn append(events: &mut Vec<Event>, payload: Payload, after_secs: i64, boot: &str) {
    let last = events.last().expect("an event");
    let event = Event::new(
        last.seq.next().expect("a sequence number"),
        Stamp {
            time: later(last.time, after_secs),
            mono: None,
        },
        BootId::from(boot.to_owned()),
        Vec::new(),
        payload,
    );
    events.push(event);
}

pub(super) fn bar(buf: &Buffer) -> Option<String> {
    rows_of(buf).iter().find_map(|row| {
        let row = row.trim();
        row.starts_with(['█', '░'])
            .then(|| row.chars().take_while(|&c| c == '█' || c == '░').collect())
    })
}
