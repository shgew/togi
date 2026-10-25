//! What one dashboard frame draws from: the folded journal, the clock, the reader's time zone and the colour
//! profile, and the few ways of writing time and core facts every panel shares.
//!
//! Every time on screen is local: each journal time passes through [`Scene::wall_second`] or
//! [`Scene::wall_minute`] and nothing else formats a time of day.

use std::borrow::Cow;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::style::Style;
use ratatui::text::Span;

use super::colours::Paint;
use crate::model::{IdleHold, Snapshot};
use crate::screen::Colours;

/// One frame's facts and clock.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Scene<'a> {
    /// The folded journal.
    pub(crate) s: Snapshot<'a>,
    /// The time the frame shows: the idle trial's start while one holds the frame, the wall clock otherwise.
    pub(crate) now: Timestamp,
    /// The reader's time zone.
    pub(crate) zone: &'a TimeZone,
    /// The colour profile.
    pub(crate) colours: Colours,
    /// The started idle trial that holds the frame still, if one runs.
    pub(crate) hold: Option<IdleHold>,
}

impl<'a> Scene<'a> {
    /// The scene of `s` at `now`. A started idle trial holds the frame at its start, so nothing on it moves with
    /// the clock until a later event releases it.
    pub(crate) fn new(
        s: Snapshot<'a>,
        now: Timestamp,
        zone: &'a TimeZone,
        colours: Colours,
    ) -> Self {
        let hold = s.idle_hold();
        Self {
            s,
            now: hold.map_or(now, |hold| hold.started),
            zone,
            colours,
            hold,
        }
    }

    /// The style of `paint` in this frame's colour profile.
    pub(crate) fn style(&self, paint: Paint) -> Style {
        paint.style(self.colours)
    }

    /// `text` in `paint`.
    pub(crate) fn paint(&self, paint: Paint, text: impl Into<Cow<'static, str>>) -> Span<'static> {
        paint.span(self.colours, text)
    }

    /// Whether a trial runs on the hardware: false once the session stopped, met a dead end, or its trial
    /// crashed or was recovered from, and between trials. Nothing is under load then, whatever the last trial
    /// loaded.
    pub(crate) fn trial_runs(&self) -> bool {
        !self.s.resting()
    }

    /// `at` as a local time of day to the second: `HH:MM:SS`.
    pub(crate) fn wall_second(&self, at: Timestamp) -> String {
        wall_second(self.zone, at)
    }

    /// `at` as a local time of day to the minute: `HH:MM`.
    pub(crate) fn wall_minute(&self, at: Timestamp) -> String {
        wall_minute(self.zone, at)
    }
}

/// `at` in `zone` as `HH:MM:SS`.
pub(crate) fn wall_second(zone: &TimeZone, at: Timestamp) -> String {
    let local = zone.to_datetime(at);
    format!(
        "{:02}:{:02}:{:02}",
        local.hour(),
        local.minute(),
        local.second()
    )
}

/// `at` in `zone` as `HH:MM`.
pub(crate) fn wall_minute(zone: &TimeZone, at: Timestamp) -> String {
    let local = zone.to_datetime(at);
    format!("{:02}:{:02}", local.hour(), local.minute())
}

/// A span as a countdown or elapsed time: `M:SS` under an hour, `H:MM:SS` from an hour.
pub(crate) fn clock(span: Duration) -> String {
    let secs = span.as_secs();
    if secs < 3600 {
        format!("{}:{:02}", secs / 60, secs % 60)
    } else {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    }
}

/// A span in hours and minutes: `2h05m`.
pub(crate) fn hm(span: Duration) -> String {
    let secs = span.as_secs();
    format!("{}h{:02}m", secs / 3600, secs / 60 % 60)
}

/// How long ago a span was: `a moment ago`, `12m ago`, `2h05m ago`.
pub(crate) fn ago(span: Duration) -> String {
    let secs = span.as_secs();
    if secs < 60 {
        "a moment ago".to_owned()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else {
        format!("{} ago", hm(span))
    }
}

/// The span from `from` to `to`, or zero when `to` is earlier.
pub(crate) fn between(from: Timestamp, to: Timestamp) -> Duration {
    let nanos = to.as_nanosecond() - from.as_nanosecond();
    crate::numbers::nonnegative_elapsed(nanos)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jiff::Timestamp;
    use jiff::tz::{TimeZone, offset};

    use super::{ago, between, clock, hm, wall_minute, wall_second};

    #[test]
    fn times_of_day_are_local() {
        let at: Timestamp = "2026-10-12T21:04:05Z".parse().expect("a timestamp");
        assert_eq!(wall_second(&TimeZone::UTC, at), "21:04:05");
        assert_eq!(wall_minute(&TimeZone::UTC, at), "21:04");
        let east = TimeZone::fixed(offset(3));
        assert_eq!(wall_second(&east, at), "00:04:05");
        assert_eq!(wall_minute(&east, at), "00:04");
    }

    #[test]
    fn spans_read_as_the_dashboard_writes_them() {
        let s = Duration::from_secs;
        assert_eq!(clock(s(0)), "0:00");
        assert_eq!(clock(s(599)), "9:59");
        assert_eq!(clock(s(3600 + 62)), "1:01:02");
        assert_eq!(hm(s(2 * 3600 + 5 * 60 + 59)), "2h05m");
        assert_eq!(ago(s(59)), "a moment ago");
        assert_eq!(ago(s(12 * 60)), "12m ago");
        assert_eq!(ago(s(3 * 3600)), "3h00m ago");
        let at: Timestamp = "2026-10-12T21:04:05Z".parse().expect("a timestamp");
        let later = at + jiff::SignedDuration::from_secs(5);
        assert_eq!(between(at, later), s(5));
        assert_eq!(between(later, at), Duration::ZERO);
    }
}
