//! The refresh clock: when a drawn frame has to be drawn again because time passed.

use std::pin::Pin;
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep};

use crate::screen::Hold;

/// What wakes the loop for the clock. Nothing is armed until a frame says how long it stays true, and a paced clock
/// also wakes at a steady interval whatever the frame says.
#[derive(Debug)]
pub(super) struct Clock {
    wall: Timestamp,
    start: Instant,
    ticker: Option<Interval>,
    timer: Option<Pin<Box<Sleep>>>,
    pace: Option<Interval>,
}

impl Clock {
    /// Tracks wall-second deadlines with monotonic timers, also waking every `tick`, if given.
    pub(super) fn paced(tick: Option<Duration>, wall: Timestamp) -> Self {
        let pace = tick.map(|tick| {
            let mut pace = tokio::time::interval_at(Instant::now() + tick, tick);
            pace.set_missed_tick_behavior(MissedTickBehavior::Delay);
            pace
        });
        Self {
            wall,
            start: Instant::now(),
            ticker: None,
            timer: None,
            pace,
        }
    }

    /// The last wall reading projected onto monotonic time, used only to notice a clock change.
    pub(super) fn now(&self) -> Timestamp {
        self.wall + self.start.elapsed()
    }

    /// Arms the clock for a frame that stays true for `hold`, as of `now`.
    ///
    /// A frozen frame arms nothing at all. A frame held until a future instant wakes once at that instant. Any
    /// other frame shows the clock, so it wakes every second.
    pub(super) fn schedule(&mut self, hold: Hold, now: Timestamp) {
        // Wall and monotonic readings are not atomic. Ignore sub-millisecond sampling jitter, but
        // re-anchor after a clock step, suspend or accelerated playback changes the wall phase.
        if now.duration_since(self.now()).unsigned_abs() > Duration::from_millis(1) {
            self.wall = now;
            self.start = Instant::now();
            self.ticker = None;
            self.timer = None;
        }
        match hold {
            Hold::Frozen => {
                self.ticker = None;
                self.timer = None;
            }
            Hold::Until(until) if until.duration_since(now).is_positive() => {
                self.ticker = None;
                let deadline = self.start + until.duration_since(self.wall).unsigned_abs();
                self.timer = Some(Box::pin(tokio::time::sleep_until(deadline)));
            }
            Hold::Never | Hold::Until(_) => {
                self.timer = None;
                if self.ticker.is_none() {
                    let second = Duration::from_secs(1);
                    let nanos = 1_000_000_000 - now.subsec_nanosecond().rem_euclid(1_000_000_000);
                    let next = now + SignedDuration::from_nanos(i64::from(nanos));
                    let deadline = self.start + next.duration_since(self.wall).unsigned_abs();
                    let mut ticker = tokio::time::interval_at(deadline, second);
                    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                    self.ticker = Some(ticker);
                }
            }
        }
    }

    /// Whether anything is armed.
    #[cfg(test)]
    pub(super) fn is_armed(&self) -> bool {
        self.ticker.is_some() || self.timer.is_some()
    }

    /// Completes when the armed timer or ticker fires, or the pace does; never completes when nothing is armed.
    pub(super) async fn wake(&mut self) {
        let Self {
            ticker,
            timer,
            pace,
            ..
        } = self;
        let held = async {
            if let Some(sleep) = timer.as_mut() {
                sleep.await;
                *timer = None;
            } else if let Some(ticker) = ticker.as_mut() {
                ticker.tick().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        match pace {
            Some(pace) => {
                tokio::select! {
                    () = held => {}
                    _ = pace.tick() => {}
                }
            }
            None => held.await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use jiff::{SignedDuration, Timestamp};
    use tokio::time::timeout;

    use super::Clock;
    use crate::screen::Hold;

    const NOW: Timestamp = Timestamp::UNIX_EPOCH;

    fn after(seconds: i64) -> Timestamp {
        NOW + SignedDuration::from_secs(seconds)
    }

    async fn wakes(clock: &mut Clock, within: Duration) -> bool {
        timeout(within, clock.wake()).await.is_ok()
    }

    #[test]
    fn redraws_follow_second_boundaries_instead_of_the_startup_phase() {
        crate::terminal::testing::paused(async {
            let now = NOW + SignedDuration::from_millis(250);
            let mut clock = Clock::paced(None, now);
            clock.schedule(Hold::Never, now);
            assert!(!wakes(&mut clock, Duration::from_millis(749)).await);
            assert!(wakes(&mut clock, Duration::from_millis(2)).await);
        });
    }

    #[test]
    fn late_rendering_skips_missed_ticks_without_shifting_the_phase() {
        crate::terminal::testing::paused(async {
            let mut clock = Clock::paced(None, NOW);
            clock.schedule(Hold::Never, clock.now());
            tokio::time::advance(Duration::from_millis(2300)).await;
            clock.wake().await;
            assert_eq!(clock.now(), NOW + SignedDuration::from_millis(2300));
            clock.schedule(Hold::Never, clock.now());
            assert!(!wakes(&mut clock, Duration::from_millis(699)).await);
            assert!(wakes(&mut clock, Duration::from_millis(2)).await);
        });
    }

    #[test_case::test_case(60_375, 600; "forward wall step")]
    #[test_case::test_case(-60_125, 600; "backward wall step")]
    #[test_case::test_case(3_600_375, 0; "suspend without monotonic advance")]
    fn changed_wall_time_rearms_the_next_wall_second(step_ms: i64, elapsed_ms: u64) {
        crate::terminal::testing::paused(async {
            let wall = NOW + SignedDuration::from_millis(250);
            let mut clock = Clock::paced(None, wall);
            clock.schedule(Hold::Never, wall);
            tokio::time::advance(Duration::from_millis(elapsed_ms)).await;
            let wall =
                wall + Duration::from_millis(elapsed_ms) + SignedDuration::from_millis(step_ms);
            clock.schedule(Hold::Never, wall);
            assert_eq!(
                clock.now(),
                wall,
                "the deadline anchor follows the new clock"
            );
            let nanos = 1_000_000_000 - wall.subsec_nanosecond().rem_euclid(1_000_000_000);
            let until = Duration::from_nanos(nanos.cast_unsigned().into());
            assert!(
                !wakes(
                    &mut clock,
                    until.checked_sub(Duration::from_millis(1)).unwrap()
                )
                .await
            );
            assert!(wakes(&mut clock, Duration::from_millis(2)).await);
        });
    }

    #[test]
    fn a_clock_frame_wakes_every_second() {
        crate::terminal::testing::paused(async {
            let mut clock = Clock::paced(None, NOW);
            clock.schedule(Hold::Never, NOW);
            assert!(!wakes(&mut clock, Duration::from_millis(999)).await);
            assert!(wakes(&mut clock, Duration::from_millis(2)).await);
            assert!(!wakes(&mut clock, Duration::from_millis(998)).await);
            assert!(wakes(&mut clock, Duration::from_millis(2)).await);
        });
    }

    #[test]
    fn a_held_frame_stays_quiet_until_its_instant_then_ticks() {
        crate::terminal::testing::paused(async {
            let mut clock = Clock::paced(None, NOW);
            clock.schedule(Hold::Never, NOW);
            clock.schedule(Hold::Until(after(10)), NOW);
            assert!(!wakes(&mut clock, Duration::from_secs(9)).await);
            assert!(wakes(&mut clock, Duration::from_secs(1)).await);
            clock.schedule(Hold::Until(after(10)), after(10));
            assert!(
                wakes(&mut clock, Duration::from_millis(1001)).await,
                "a passed hold ticks"
            );
        });
    }

    #[test]
    fn a_frozen_frame_arms_nothing_for_hours() {
        crate::terminal::testing::paused(async {
            let mut clock = Clock::paced(None, NOW);
            clock.schedule(Hold::Never, NOW);
            clock.schedule(Hold::Frozen, NOW);
            assert!(!clock.is_armed());
            assert!(!wakes(&mut clock, Duration::from_hours(6)).await);
        });
    }

    #[test]
    fn the_ticker_is_not_restarted_by_a_redraw() {
        crate::terminal::testing::paused(async {
            let mut clock = Clock::paced(None, NOW);
            clock.schedule(Hold::Never, NOW);
            tokio::time::advance(Duration::from_millis(600)).await;
            clock.schedule(Hold::Never, NOW + SignedDuration::from_millis(600));
            assert!(wakes(&mut clock, Duration::from_millis(401)).await);
        });
    }
}
