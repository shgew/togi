//! Helpers the panel tests share.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::text::Line;

use super::super::fit::plain;
use super::super::scene::Scene;
use crate::model::Snapshot;
use crate::screen::Colours;

/// The rows as the screen shows them, without their style.
pub(super) fn texts(lines: &[Line<'_>], colours: Colours) -> Vec<String> {
    lines.iter().map(|line| plain(line, colours)).collect()
}

pub(super) fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).expect("a timestamp")
}

/// `s` as a plain frame at second 2000 in `zone`.
pub(super) fn scene<'a>(s: &Snapshot<'a>, zone: &'a TimeZone) -> Scene<'a> {
    Scene::new(*s, at(2000), zone, Colours::None)
}
