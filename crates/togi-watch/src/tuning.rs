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
    crate::numbers::nonnegative_elapsed(to.as_nanosecond() - from.as_nanosecond())
}

/// The run that has started and not ended.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
struct OpenRun {
    /// The boot of the run's latest `config.loaded`: one on a later boot continues the run.
    boot: BootId,
    /// The boot and time of the run's latest event.
    last_boot: BootId,
    last: Timestamp,
    startup: Option<Startup>,
}

/// What the unbroken chain of `mce` and `boot.leave_reason` events that ends a run's latest event has put in
/// doubt. A starting session records those before its `config.loaded` (the kernel boundary, then the boot
/// handoff's leave reason), but a stopping one records an `mce` too, and the journal cannot tell the two apart
/// (both are placed between trials, with no trial and no boot). So the chain is provisional: a `config.loaded` in
/// the run's own boot is treated as the startup of a restart and takes the chain's silence back, which also
/// drops a stop-time `mce`'s time when the run was killed before its `shutdown` and restarted in the same boot.
/// Any other event confirms the chain.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
struct Startup {
    /// The counted time before the chain.
    counted: Duration,
    /// The run's last event before the chain, when the chain began on a later boot than it: that boot's recovery
    /// is unresolved until a `config.loaded` confirms the boot, so the whole interval to it counts at most
    /// [`RECOVERY_LIMIT`].
    recovery_from: Option<Timestamp>,
}

/// The tuning time as of the latest `trial.start`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
struct TrialStart {
    at: Timestamp,
    tuning: Duration,
}

/// The time spent inside runs, from the events folded so far.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct TuningClock {
    /// The length of the runs and recoveries that ended or whose events are all folded.
    counted: Duration,
    open: Option<OpenRun>,
    /// What a frozen frame shows while the trial it started holds the screen.
    trial_start: Option<TrialStart>,
}

impl TuningClock {
    pub(crate) fn fold(&mut self, event: &Event) {
        match event.payload {
            Payload::SessionStart(_) => *self = Self::default(),
            // A reset is recorded outside any run, with no `config.loaded` or `shutdown` around it, so it neither
            // ends a run nor shows one alive.
            Payload::CommandReset(_)
            // A day observer runs outside tuning: its events, written in any boot, neither extend a run nor mark
            // a reboot inside one.
            | Payload::DayBootStart(_)
            | Payload::UnitMapping(_)
            | Payload::UnitEdge(_)
            | Payload::ClimbDecision(_)
            | Payload::UnitStop(_)
            | Payload::CampaignStart(_)
            | Payload::CampaignCharge(_)
            | Payload::CampaignReclassified(_)
            | Payload::FirmwareRestoration(_)
            | Payload::SuccessorImported(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::SessionImported(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayApplied(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::DayUncleanEnd(_) => {}
            Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::TaskStart(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => {
                self.advance(event);
                if matches!(event.payload, Payload::TrialStart(_)) {
                    self.trial_start = Some(TrialStart {
                        at: event.time,
                        tuning: self.counted,
                    });
                }
            }
        }
    }

    fn advance(&mut self, event: &Event) {
        let loaded = matches!(event.payload, Payload::ConfigLoaded(_));
        let Some(mut run) = self.open.take() else {
            if loaded {
                self.open = Some(OpenRun {
                    boot: event.boot.clone(),
                    last_boot: event.boot.clone(),
                    last: event.time,
                    startup: None,
                });
            }
            return;
        };
        let before = self.counted;
        let startup = run.startup.take();
        let rebooted = !run.last_boot.as_str().is_empty()
            && !event.boot.as_str().is_empty()
            && run.last_boot != event.boot;
        // A `config.loaded` in the run's own boot restarts the run without a shutdown: its time up to the last
        // event before the restart's startup stands and the silence before the restart belongs to no run.
        let restarted = loaded && run.boot == event.boot;
        if restarted {
            self.counted = startup.as_ref().map_or(self.counted, |chain| chain.counted);
        } else if let (
            true,
            Some(Startup {
                counted,
                recovery_from: Some(from),
            }),
        ) = (loaded, &startup)
        {
            self.counted = *counted + between(*from, event.time).min(RECOVERY_LIMIT);
            run.boot.clone_from(&event.boot);
        } else {
            let gap = between(run.last, event.time);
            // The first `config.loaded` of a later boot ends that boot's recovery, however its events before the
            // configuration were spaced.
            let confirms_boot = loaded
                && !run.boot.as_str().is_empty()
                && !event.boot.as_str().is_empty()
                && run.boot != event.boot;
            self.counted += if rebooted || confirms_boot {
                gap.min(RECOVERY_LIMIT)
            } else {
                gap
            };
            if loaded {
                run.boot.clone_from(&event.boot);
            }
        }
        run.startup = if matches!(event.payload, Payload::Mce(_) | Payload::BootLeaveReason(_)) {
            Some(match startup {
                Some(chain) if chain.recovery_from.is_some() || !rebooted => chain,
                _ => Startup {
                    counted: before,
                    recovery_from: rebooted.then_some(run.last),
                },
            })
        } else {
            None
        };
        run.last = event.time;
        run.last_boot.clone_from(&event.boot);
        if !matches!(event.payload, Payload::Shutdown(_) | Payload::DeadEnd(_)) {
            self.open = Some(run);
        }
    }

    /// The tuning time as of `at`: a run still open counts up to `at`, at most [`RECOVERY_LIMIT`] past its last
    /// event. A frame frozen by an idle trial uses [`Self::at_latest_trial_start`] instead, since a later `at`
    /// cannot take back the events folded since.
    pub fn at(&self, at: Timestamp) -> Duration {
        let tail = self.open.as_ref().map_or(Duration::ZERO, |run| {
            between(run.last, at).min(RECOVERY_LIMIT)
        });
        self.counted + tail
    }

    /// The tuning time as of the latest `trial.start`, which began at `started`: what a frame frozen by that trial
    /// shows however many events the trial has recorded since. Only the latest start is kept; for any other
    /// instant this is [`Self::at`].
    pub fn at_latest_trial_start(&self, started: Timestamp) -> Duration {
        self.trial_start
            .as_ref()
            .filter(|start| start.at == started)
            .map_or_else(|| self.at(started), |start| start.tuning)
    }
}

#[cfg(test)]
mod tests {
    use togi_journal::{
        BootLeaveReason, CommandReset, DeadEnd, DeadEndCondition, Mce, ProfileRestored,
        ResetTarget, Shutdown, ShutdownReason, Stamp,
    };
    use togi_machine::{BankType, CpuId};

    use super::*;
    use crate::tests::kit::{config_loaded, core, crash, offsets, seq, session_start, warning};

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

    /// A machine check as the startup or stop kernel boundary records it: between trials, no trial's.
    fn mce() -> Payload {
        Mce {
            cpu: CpuId::from(0),
            core: Some(core(0)),
            bank: 0,
            bank_type: BankType::LoadStore,
            corrected: true,
            monotonic_ns: None,
            from_boot: None,
            trial: None,
            between_trials: true,
            lines: Vec::new(),
        }
        .into()
    }

    /// The leave reason the boot handoff records after the first durable append of a run's startup.
    fn leave_reason() -> Payload {
        BootLeaveReason {
            reason_id: "r1".to_owned(),
            restart_limit_count: 1,
            reason: "restart limit".to_owned(),
        }
        .into()
    }

    /// `togi reset` for a core: recorded outside any run, with no cause.
    fn reset() -> Payload {
        CommandReset {
            target: ResetTarget::Core(core(0)),
        }
        .into()
    }

    fn restored() -> Payload {
        ProfileRestored {
            offsets: offsets(&[0]),
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

    #[test]
    fn a_restart_leaves_out_the_silence_before_its_startup_machine_checks() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event before the kill")),
            (5000, "a", mce()),
            (5000, "a", leave_reason()),
            (5001, "a", mce()),
            (5001, "a", config_loaded()),
            (5101, "a", shutdown()),
        ]);
        assert_eq!(secs(&clock, 9000), 200);
    }

    #[test]
    fn a_machine_check_recorded_before_the_machine_died_counts_up_to_it() {
        let open = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the trial")),
            (4000, "a", mce()),
        ]);
        assert_eq!(secs(&open, 4000), 3990);
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the trial")),
            (4000, "a", mce()),
            (4000 + DAYS, "b", config_loaded()),
            (4100 + DAYS, "b", shutdown()),
        ]);
        assert_eq!(secs(&clock, 4100 + DAYS), 3990 + LIMIT + 100);
    }

    #[test]
    fn startup_machine_checks_on_the_next_boot_count_as_its_recovery() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the crashed boot")),
            (110 + DAYS, "b", mce()),
            (110 + DAYS, "b", leave_reason()),
            (111 + DAYS, "b", config_loaded()),
            (211 + DAYS, "b", shutdown()),
        ]);
        assert_eq!(secs(&clock, 211 + DAYS), 100 + LIMIT + 100);
    }

    #[test]
    fn a_configuration_days_after_the_next_boots_startup_evidence_stays_inside_one_recovery() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the crashed boot")),
            (110 + DAYS, "b", mce()),
            (110 + DAYS, "b", leave_reason()),
            (110 + 2 * DAYS, "b", config_loaded()),
            (210 + 2 * DAYS, "b", shutdown()),
        ]);
        assert_eq!(secs(&clock, 210 + 2 * DAYS), 100 + LIMIT + 100);
        let plain = clock_of_events_without_chain();
        assert_eq!(secs(&plain, 210 + 2 * DAYS), 100 + 2 * LIMIT + 100);
    }

    /// A first event of the next boot that is no startup evidence, then a configuration days later: each gap is
    /// bounded on its own.
    fn clock_of_events_without_chain() -> TuningClock {
        clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the crashed boot")),
            (110 + DAYS, "b", warning("first event of the next boot")),
            (110 + 2 * DAYS, "b", config_loaded()),
            (210 + 2 * DAYS, "b", shutdown()),
        ])
    }

    #[test]
    fn a_terminal_machine_check_keeps_its_time_when_the_next_boots_evidence_follows() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the trial")),
            (4000, "a", mce()),
            (4000 + DAYS, "b", mce()),
            (4005 + DAYS, "b", config_loaded()),
            (4105 + DAYS, "b", shutdown()),
        ]);
        assert_eq!(secs(&clock, 4105 + DAYS), 3990 + LIMIT + 100);
    }

    #[test]
    fn a_reset_on_a_later_boot_neither_ends_nor_extends_a_crashed_run() {
        let reset_only = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the crashed boot")),
            (110 + DAYS, "b", reset()),
        ]);
        assert_eq!(secs(&reset_only, 110 + DAYS), 100 + LIMIT);
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event of the crashed boot")),
            (110 + DAYS, "b", reset()),
            (111 + 2 * DAYS, "b", config_loaded()),
            (211 + 2 * DAYS, "b", shutdown()),
        ]);
        assert_eq!(secs(&clock, 211 + 2 * DAYS), 100 + LIMIT + 100);
    }

    #[test]
    fn a_reset_in_the_runs_own_boot_leaves_the_restart_silence_out() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event before the kill")),
            (110 + DAYS, "a", reset()),
            (111 + 2 * DAYS, "a", config_loaded()),
            (211 + 2 * DAYS, "a", shutdown()),
        ]);
        assert_eq!(secs(&clock, 211 + 2 * DAYS), 200);
    }

    #[test]
    fn a_clean_stop_stays_closed_through_its_trailing_events_and_the_next_run() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("w")),
            (200, "a", restored()),
            (210, "a", shutdown()),
            (211, "a", restored()),
            (300, "a", warning("after the stop")),
            (4999, "a", mce()),
            (4999, "a", leave_reason()),
        ]);
        assert_eq!(secs(&clock, 4999), 200);
        assert_eq!(secs(&clock, 4999 + DAYS), 200);
        let mut clock = clock;
        clock.fold(&event(5000, "a", config_loaded()));
        clock.fold(&event(5100, "a", shutdown()));
        assert_eq!(secs(&clock, 90_000), 300);
    }

    #[test]
    fn a_confirmed_startup_is_not_rolled_back_by_a_later_restart() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event before the kill")),
            (5000, "a", mce()),
            (5001, "a", config_loaded()),
            (5101, "a", warning("ordinary runtime")),
            (5201, "a", warning("more ordinary runtime")),
            (9000, "a", config_loaded()),
            (9100, "a", shutdown()),
        ]);
        assert_eq!(secs(&clock, 90_000), 400);
    }

    #[test]
    fn an_event_after_a_machine_check_confirms_it_before_a_same_boot_restart() {
        let clock = clock(&[
            (10, "a", config_loaded()),
            (110, "a", warning("last event before the machine check")),
            (5000, "a", mce()),
            (5010, "a", warning("the run went on")),
            (5500, "a", config_loaded()),
            (5600, "a", shutdown()),
        ]);
        assert_eq!(secs(&clock, 90_000), 5100);
    }
}
