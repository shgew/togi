//! Tuning time: how long togi ran, folded one event at a time, as the dashboard header shows it beside the
//! session's age.
//!
//! A run is what `just stats` calls a run: it starts at a `config.loaded` and spans its crash reboots until a
//! `shutdown`, so the time a crash reboot takes counts. The time between a clean shutdown and the next run does
//! not, nor does the time between a run's dead end and the next run.
//!
//! A crash reboot is the one gap whose length togi does not bound: a machine left crashed for days and then
//! resumed shows the same boundary as one that rebooted in a minute. [`RECOVERY_LIMIT`] bounds what such a gap
//! adds, and the same bound caps what a run with no recorded end adds beyond its last event, since an open run
//! may be a crashed machine waiting for its recovery.

use std::time::Duration;

use jiff::Timestamp;
use togi_journal::{Event, Payload};
use togi_machine::BootId;

/// The most one crash reboot adds to the tuning time: 30 minutes, the longest wait a live run schedules on its own
/// (a backend's third retry waits 1800 s). A reboot, the watchdog's 30 s and the resumed run's startup took 52 to
/// 157 s in every recovery of the long target-machine session this was chosen from; a gap past the limit is an
/// absence, not a recovery, and counting all of it would credit togi with the days a crashed machine sat idle.
pub const RECOVERY_LIMIT: Duration = Duration::from_mins(30);

fn between(from: Timestamp, to: Timestamp) -> Duration {
    Duration::try_from(to.duration_since(from)).unwrap_or(Duration::ZERO)
}

/// The run that has started and not ended.
#[derive(Debug, Clone)]
struct OpenRun {
    /// The boot of the run's latest `config.loaded`: one on a later boot continues the run.
    boot: BootId,
    /// The boot and time of the run's latest event.
    last_boot: BootId,
    last: Timestamp,
}

/// The time spent inside runs, from the events folded so far.
#[derive(Debug, Clone, Default)]
pub struct TuningClock {
    /// The length of the runs and recoveries that ended or whose events are all folded.
    counted: Duration,
    open: Option<OpenRun>,
}

impl TuningClock {
    pub(crate) fn fold(&mut self, event: &Event) {
        if matches!(event.payload, Payload::SessionStart(_)) {
            *self = Self::default();
            return;
        }
        let loaded = matches!(event.payload, Payload::ConfigLoaded(_));
        let Some(mut run) = self.open.take() else {
            if loaded {
                self.open = Some(OpenRun {
                    boot: event.boot.clone(),
                    last_boot: event.boot.clone(),
                    last: event.time,
                });
            }
            return;
        };
        // A `config.loaded` in the run's own boot restarts the run without a shutdown: its time up to the last
        // event stands and the silence before the restart belongs to no run.
        let restarted = loaded && run.boot == event.boot;
        if !restarted {
            let gap = between(run.last, event.time);
            let rebooted = !run.last_boot.as_str().is_empty()
                && !event.boot.as_str().is_empty()
                && run.last_boot != event.boot;
            self.counted += if rebooted {
                gap.min(RECOVERY_LIMIT)
            } else {
                gap
            };
            if loaded {
                run.boot.clone_from(&event.boot);
            }
        }
        run.last = event.time;
        run.last_boot.clone_from(&event.boot);
        if !matches!(event.payload, Payload::Shutdown(_) | Payload::DeadEnd(_)) {
            self.open = Some(run);
        }
    }

    /// The tuning time as of `at`: a run still open counts up to `at`, at most [`RECOVERY_LIMIT`] past its last
    /// event. A caller that stands for a frozen frame passes the instant it froze at.
    pub fn at(&self, at: Timestamp) -> Duration {
        let tail = self.open.as_ref().map_or(Duration::ZERO, |run| {
            between(run.last, at).min(RECOVERY_LIMIT)
        });
        self.counted + tail
    }
}

#[cfg(test)]
mod tests {
    use togi_journal::{DeadEnd, DeadEndCondition, Shutdown, ShutdownReason, Stamp};

    use super::*;
    use crate::tests::kit::{config_loaded, crash, seq, session_start, warning};

    const LIMIT: u64 = RECOVERY_LIMIT.as_secs();
    const DAYS: i64 = 3 * 24 * 3600;

    fn event(second: i64, boot: &str, payload: Payload) -> Event {
        let time = Timestamp::from_second(1_700_000_000 + second).unwrap();
        Event::new(
            seq(1),
            Stamp { time, mono: None },
            BootId::from(boot.to_owned()),
            Vec::new(),
            payload,
        )
    }

    fn shutdown() -> Payload {
        Shutdown {
            reason: ShutdownReason::Signal,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }
        .into()
    }

    fn dead_end() -> Payload {
        DeadEnd {
            condition: DeadEndCondition::Smu,
            core: None,
            detail: "core 00 read back 0".to_owned(),
            action: None,
        }
        .into()
    }

    fn clock(events: &[(i64, &str, Payload)]) -> TuningClock {
        let mut clock = TuningClock::default();
        clock.fold(&event(0, "a", session_start(&[(0, 0)])));
        for (second, boot, payload) in events {
            clock.fold(&event(*second, boot, payload.clone()));
        }
        clock
    }

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(1_700_000_000 + second).unwrap()
    }

    fn secs(clock: &TuningClock, second: i64) -> u64 {
        clock.at(at(second)).as_secs()
    }

    #[test]
    fn nothing_runs_before_the_first_config() {
        let clock = clock(&[(100, "a", warning("early"))]);
        assert_eq!(secs(&clock, 5000), 0);
    }

    #[test]
    fn the_time_between_a_clean_shutdown_and_the_next_run_is_not_counted() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (610, "a", warning("w")),
            (1210, "a", shutdown()),
            (11_210, "b", config_loaded()),
            (11_810, "b", shutdown()),
            (20_000, "b", warning("after the stop")),
        ]);
        assert_eq!(secs(&clock, 20_000), 1800);
        assert_eq!(
            secs(&clock, 90_000),
            1800,
            "a stopped session adds nothing as time passes"
        );
    }

    #[test]
    fn a_crash_reboot_counts_and_the_run_goes_on_across_it() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the crashed boot")),
            (170, "b", warning("first event of the next boot")),
            (171, "b", config_loaded()),
            (172, "b", crash("a", None).into()),
            (271, "b", shutdown()),
        ]);
        assert_eq!(secs(&clock, 10_000), 261);
    }

    #[test]
    fn a_run_left_crashed_for_days_adds_only_the_recovery_limit() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the crashed boot")),
            (110 + DAYS, "b", warning("resumed days later")),
            (111 + DAYS, "b", config_loaded()),
            (211 + DAYS, "b", shutdown()),
        ]);
        assert_eq!(secs(&clock, 211 + DAYS), 100 + LIMIT + 1 + 100);
    }

    #[test]
    fn a_gap_inside_a_boot_counts_whole() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (10 + 7200, "a", warning("after a long quiet trial")),
            (20 + 7200, "a", shutdown()),
        ]);
        assert_eq!(secs(&clock, 90_000), 7210);
    }

    #[test]
    fn a_restart_inside_a_boot_leaves_out_the_silence_before_it() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event before the kill")),
            (5000, "a", config_loaded()),
            (5100, "a", shutdown()),
        ]);
        assert_eq!(secs(&clock, 9000), 200);
    }

    #[test]
    fn an_open_run_counts_up_to_now_but_not_past_the_recovery_limit() {
        let clock = clock(&[(10, "a", config_loaded()), (110, "a", warning("w"))]);
        assert_eq!(secs(&clock, 110), 100);
        assert_eq!(secs(&clock, 160), 150);
        assert_eq!(secs(&clock, 110 + DAYS), 100 + LIMIT);
        assert_eq!(
            secs(&clock, 50),
            100,
            "an instant before the last event adds nothing"
        );
    }

    #[test]
    fn a_dead_end_ends_the_run_and_freezes_the_time() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (310, "a", dead_end()),
            (400, "a", shutdown()),
        ]);
        assert_eq!(secs(&clock, 310), 300);
        assert_eq!(secs(&clock, 90_000), 300);
    }

    #[test]
    fn a_new_session_starts_the_clock_over() {
        let mut clock = clock(&[(10, "a", config_loaded()), (110, "a", shutdown())]);
        assert_eq!(secs(&clock, 110), 100);
        clock.fold(&event(200, "a", session_start(&[(0, 0)])));
        assert_eq!(secs(&clock, 5000), 0);
    }
}
