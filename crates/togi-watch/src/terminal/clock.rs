//! The refresh clock: when a drawn frame has to be drawn again because time passed.

use std::pin::Pin;
use std::time::Duration;

use jiff::Timestamp;
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep};

use crate::screen::Hold;

/// What wakes the loop for the clock. Nothing is armed until a frame says how long it stays true, and a paced clock
/// also wakes at a steady interval whatever the frame says.
#[derive(Debug, Default)]
pub(super) struct Clock {
    ticker: Option<Interval>,
    timer: Option<Pin<Box<Sleep>>>,
    pace: Option<Interval>,
}

impl Clock {
    /// A clock that also wakes every `tick`, if given.
    pub(super) fn paced(tick: Option<Duration>) -> Self {
        let pace = tick.map(|tick| {
            let mut pace = tokio::time::interval_at(Instant::now() + tick, tick);
            pace.set_missed_tick_behavior(MissedTickBehavior::Delay);
            pace
        });
        Self {
            pace,
            ..Self::default()
        }
    }

    /// Arms the clock for a frame that stays true for `hold`, as of `now`.
    ///
    /// A frozen frame arms nothing at all. A frame held until a future instant wakes once at that instant. Any
    /// other frame shows the clock, so it wakes every second.
    pub(super) fn schedule(&mut self, hold: Hold, now: Timestamp) {
        match hold {
            Hold::Frozen => {
                self.ticker = None;
                self.timer = None;
            }
            Hold::Until(until) if until.duration_since(now).is_positive() => {
                self.ticker = None;
                let delay = until.duration_since(now).unsigned_abs();
                self.timer = Some(Box::pin(tokio::time::sleep(delay)));
            }
            Hold::Never | Hold::Until(_) => {
                self.timer = None;
                if self.ticker.is_none() {
                    let second = Duration::from_secs(1);
                    let mut ticker = tokio::time::interval_at(Instant::now() + second, second);
                    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
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
    fn a_clock_frame_wakes_every_second() {
        crate::terminal::testing::paused(async {
            let mut clock = Clock::default();
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
            let mut clock = Clock::default();
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
            let mut clock = Clock::default();
            clock.schedule(Hold::Never, NOW);
            clock.schedule(Hold::Frozen, NOW);
            assert!(!clock.is_armed());
            assert!(!wakes(&mut clock, Duration::from_hours(6)).await);
        });
    }

    #[test]
    fn the_ticker_is_not_restarted_by_a_redraw() {
        crate::terminal::testing::paused(async {
            let mut clock = Clock::default();
            clock.schedule(Hold::Never, NOW);
            tokio::time::advance(Duration::from_millis(600)).await;
            clock.schedule(Hold::Never, NOW);
            assert!(wakes(&mut clock, Duration::from_millis(401)).await);
        });
    }
}
