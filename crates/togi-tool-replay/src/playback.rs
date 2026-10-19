//! The playback clock: which recorded events a stretch of real time has played, and what time the dashboard shows.

use std::str::FromStr;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use togi_journal::Event;

/// How many simulated seconds pass per real second: finite and positive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Speed(f64);

/// A speed that is not finite and positive.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("--speed must be finite and positive")]
pub struct SpeedError;

impl Speed {
    pub fn new(factor: f64) -> Result<Self, SpeedError> {
        if factor.is_finite() && factor > 0.0 {
            Ok(Self(factor))
        } else {
            Err(SpeedError)
        }
    }

    fn scale(self, elapsed: Duration) -> Duration {
        saturating_playback_elapsed(elapsed.as_secs_f64() * self.0)
    }
}

impl FromStr for Speed {
    type Err = SpeedError;

    fn from_str(text: &str) -> Result<Self, SpeedError> {
        text.parse().map_err(|_| SpeedError).and_then(Self::new)
    }
}

/// Playback continues at the largest displayable elapsed time when scaling overflows.
fn saturating_playback_elapsed(seconds: f64) -> Duration {
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX)
}

fn saturating_display_elapsed(elapsed: Duration) -> SignedDuration {
    SignedDuration::try_from(elapsed).unwrap_or(SignedDuration::MAX)
}

/// How many events come at or before `seq`, and at least one.
pub fn prefix(events: &[Event], seq: u64) -> usize {
    events
        .iter()
        .take_while(|event| event.seq.get() <= seq)
        .count()
        .max(1)
}

/// `time` moved by `by`, held at the ends of the timeline instead of overflowing it.
pub fn offset(time: Timestamp, by: SignedDuration) -> Timestamp {
    time.checked_add(by).unwrap_or(if by.is_negative() {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    })
}

/// How long the recording took to get from `before` to `after`: the monotonic difference when both events carry a
/// stamp from the same known boot, else the wall-clock difference. Neither runs backwards, so a clock correction
/// costs no time and never reorders anything.
fn gap(before: &Event, after: &Event) -> Duration {
    let same_boot = !before.boot.as_str().is_empty() && before.boot == after.boot;
    match (before.mono, after.mono) {
        (Some(before), Some(after)) if same_boot => after.saturating_sub(before),
        _ => after
            .time
            .duration_since(before.time)
            .try_into()
            .unwrap_or(Duration::ZERO),
    }
}

/// A recording played forward from a prefix of its events.
#[derive(Debug)]
pub struct Playback<'a> {
    events: &'a [Event],
    speed: Speed,
    shown: usize,
    at: Duration,
    gap: Duration,
}

impl<'a> Playback<'a> {
    /// Starts with the first `shown` events (at least one, at most all) already played.
    pub fn new(events: &'a [Event], shown: usize, speed: Speed) -> Self {
        let mut playback = Self {
            events,
            speed,
            shown: shown.clamp(1, events.len().max(1)),
            at: Duration::ZERO,
            gap: Duration::ZERO,
        };
        playback.schedule();
        playback
    }

    fn schedule(&mut self) {
        if let Some(after) = self.events.get(self.shown) {
            self.gap = gap(&self.events[self.shown - 1], after);
        }
    }

    /// Plays on to `elapsed` of real time since playback started, which is the only clock it reads: the recording's
    /// own wall clock may have been corrected. Returns how many events are played and the time to show, the last
    /// played event's time plus what has passed since it.
    pub fn advance(&mut self, elapsed: Duration) -> (usize, Timestamp) {
        let simulated = self.speed.scale(elapsed);
        while self.shown < self.events.len() && self.gap <= simulated.saturating_sub(self.at) {
            self.at = self.at.saturating_add(self.gap);
            self.shown += 1;
            self.schedule();
        }
        let since = simulated.saturating_sub(self.at);
        let since = saturating_display_elapsed(since);
        (self.shown, offset(self.events[self.shown - 1].time, since))
    }
}

#[cfg(test)]
mod numeric_tests {
    use super::*;
    use test_case::test_case;

    #[test_case(0.0, Duration::ZERO; "zero")]
    #[test_case(1.0, Duration::from_secs(1); "exact second")]
    #[test_case(f64::MAX, Duration::MAX; "finite overflow")]
    #[test_case(f64::INFINITY, Duration::MAX; "scaling overflow")]
    fn playback_elapsed_saturates(seconds: f64, expected: Duration) {
        assert_eq!(saturating_playback_elapsed(seconds), expected);
    }

    #[test_case(Duration::ZERO, SignedDuration::ZERO; "zero")]
    #[test_case(Duration::from_secs(i64::MAX.cast_unsigned()), SignedDuration::from_secs(i64::MAX); "largest exact seconds")]
    #[test_case(Duration::MAX, SignedDuration::MAX; "display overflow")]
    fn display_elapsed_saturates(elapsed: Duration, expected: SignedDuration) {
        assert_eq!(saturating_display_elapsed(elapsed), expected);
    }
}
