//! Whole frames through [`render`](super::render): each test draws a journal on a screen and pins what the frame
//! shows, how it is bounded and how long it holds.

mod arrivals;
mod bands;
mod bounds;
mod escapes;
mod holds;
mod live_panels;
mod local_time;
mod pages;
mod panels;
mod without_journal;

use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use ratatui::buffer::Buffer;
use togi_journal::{Event, Payload, Stamp};
use togi_machine::BootId;
use togi_watchtest::Scenario;

use super::render;
use super::testing::{draw_checked, rows_of, screen};
use crate::model::{Model, project};
use crate::screen::{Colours, Drawn, Journal, Screen, View};

pub(super) const SIZES: [(u16, u16); 3] = [(240, 67), (160, 45), (120, 33)];
/// Every size the frames are checked at: the classes' boundaries on both sides, the numeric table's 80 by 30 and
/// the scrolled form under it.
pub(super) const ALL_SIZES: [(u16, u16); 8] = [
    (240, 67),
    (160, 45),
    (159, 45),
    (160, 44),
    (120, 33),
    (80, 30),
    (80, 24),
    (79, 29),
];
pub(super) const VIEWS: [View; 9] = View::ALL;
pub(super) const PROFILES: [Colours; 3] = [Colours::Full, Colours::Console, Colours::None];

pub(super) struct Fixture {
    pub(super) model: Model,
    pub(super) now: Timestamp,
}

impl Fixture {
    pub(super) fn scenario(scenario: Scenario) -> Self {
        Self::of(&scenario.events().expect("the scenario's journal"))
    }

    pub(super) fn of(events: &[Event]) -> Self {
        let last = events.last().expect("an event").time;
        Self {
            model: project(events),
            now: later(last, 40),
        }
    }

    pub(super) fn journal(&self) -> Journal<'_> {
        ready(&self.model)
    }

    pub(super) fn draw(&self, screen: &Screen, now: Timestamp) -> (Buffer, Drawn) {
        draw_journal(&self.journal(), screen, now, &TimeZone::UTC)
    }

    pub(super) fn main(&self, width: u16, height: u16) -> (Buffer, Drawn) {
        self.draw(&main_screen(width, height), self.now)
    }
}

pub(super) fn ready(model: &Model) -> Journal<'_> {
    let snapshot = model.snapshot();
    Journal::Ready {
        snapshot,
        log: snapshot.log(),
    }
}

pub(super) fn some_time() -> Timestamp {
    Timestamp::from_second(1_700_000_000).expect("a time")
}

pub(super) fn later(time: Timestamp, secs: i64) -> Timestamp {
    time.checked_add(SignedDuration::from_secs(secs))
        .expect("a time")
}

pub(super) fn main_screen(width: u16, height: u16) -> Screen {
    screen(View::Main, width, height, Colours::None, true)
}

pub(super) fn one_frame(width: u16, height: u16) -> Screen {
    Screen {
        one_frame: true,
        keys: false,
        ..main_screen(width, height)
    }
}

pub(super) fn draw_journal(
    journal: &Journal<'_>,
    screen: &Screen,
    now: Timestamp,
    zone: &TimeZone,
) -> (Buffer, Drawn) {
    draw_checked(screen, |screen, buf| {
        render(journal, screen, now, zone, buf)
    })
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
