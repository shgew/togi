use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use jiff::Timestamp;
use test_case::test_case;
use togi_journal::fs::{Fault, Op};
use togi_journal::{DayFault, EventKind, LiveFault, LoadMix, Trap};
use togi_journal::{MemFs, SessionId, SessionStart, StateDir};
use togi_machine::{
    BankType, BiosContext, CcdId, CoreInfo, CpuId, Error as MachineError, KernelCursor,
    KernelError, KernelErrorKind, KernelRead, Mce, Partial, ProcessFault, ProcessFaultKind,
    ResetKind,
};

use super::*;
mod applying;

fn offsets(value: i64) -> PerCore<ObservedOffset> {
    [(CoreId::new(0).unwrap(), ObservedOffset(value))]
        .into_iter()
        .collect()
}

fn bios() -> BiosContext {
    BiosContext {
        bios_version: "test".into(),
        board: "test board".into(),
        cpu_model: "test CPU".into(),
        microcode: "test".into(),
        boost_limit_mhz: 5000,
    }
}

fn state_dir(fs: Arc<MemFs>) -> StateDir {
    #[expect(
        clippy::disallowed_methods,
        reason = "observer lifecycle tests inject an in-memory journal"
    )]
    StateDir::lock_with_fs(fs, Path::new("/state")).unwrap()
}

pub(super) fn journal() -> Journal {
    journal_on(Arc::new(MemFs::new()))
}

fn journal_on(fs: Arc<MemFs>) -> Journal {
    let mut journal = state_dir(fs)
        .open_at("day-a".to_owned().into(), Timestamp::UNIX_EPOCH)
        .unwrap();
    journal
        .append(
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: Some(Duration::ZERO),
            },
            SessionStart {
                build: current_build(),
                session: SessionId::new(Timestamp::UNIX_EPOCH, None).unwrap(),
                cores: vec![CoreInfo {
                    core: CoreId::new(0).unwrap(),
                    ccd: CcdId::new(0).unwrap(),
                    cpus: vec![CpuId::from(0_u16)],
                }],
            }
            .into(),
            &[],
        )
        .unwrap();
    journal
}

#[test]
fn a_close_failure_and_emergency_zero_failure_report_both_causes_in_order() {
    let fs = Arc::new(MemFs::new());
    let journal = journal_on(Arc::clone(&fs));
    let before = journal.events().to_vec();
    let mut hardware = applying::Hardware::stock();
    hardware.current = -10;
    hardware.failure = applying::Failure::Emergency;
    fs.inject(Fault::before(Op::Sync, 1).named("events.jsonl"));

    let error = close_failure(journal.close().unwrap_err(), &mut hardware);
    assert_eq!(
        render_error(&error),
        "emergency zero after daily service failure: close journal: injected Sync fault; emergency zero: injected emergency zero fault"
    );
    assert_eq!(hardware.emergency, 1);
    assert_eq!(hardware.current, -10);
    let reopened = state_dir(fs)
        .open_at("day-a".to_owned().into(), Timestamp::UNIX_EPOCH)
        .unwrap();
    assert_eq!(reopened.events(), before);
    assert!(!reopened.events().iter().any(|event| matches!(
        event.kind(),
        EventKind::DayCleanStop | EventKind::ProfileRestored
    )));
}

type LiveLog = dyn FnMut(Option<&KernelCursor>) -> Result<KernelRead, Partial<KernelRead>>;

struct Reading {
    reason: Option<ResetKind>,
    watchdog: VecDeque<WatchdogState>,
    live: Box<LiveLog>,
    /// Whether a read under a cancelled token fails like the hardware read, which gives up its wait.
    /// Off by default: a read that finished before the signal still returns its scripted state.
    cancellable: bool,
}

fn armed() -> WatchdogState {
    WatchdogState::Armed {
        detail: "watchdog0 (SP5100 TCO timer) active, held by PID 1".into(),
    }
}

impl Reading {
    fn fixed() -> Self {
        Self {
            reason: None,
            watchdog: [armed()].into(),
            live: Box::new(|_| Ok(KernelRead::default())),
            cancellable: false,
        }
    }

    fn logging(
        mut self,
        live: impl FnMut(Option<&KernelCursor>) -> Result<KernelRead, Partial<KernelRead>> + 'static,
    ) -> Self {
        self.live = Box::new(live);
        self
    }

    fn reset_reason_after(&self) -> Result<ResetReason, MachineError> {
        self.reason
            .map(|kind| ResetReason {
                kind: Some(kind),
                raw: vec!["reset evidence".into()],
                supported: true,
            })
            .ok_or(MachineError::BootMissing)
    }
    fn watchdog(&mut self, cancel: &CancellationToken) -> Result<WatchdogState, MachineError> {
        if self.cancellable && cancel.is_cancelled() {
            return Err(MachineError::Cancelled);
        }
        Ok(if self.watchdog.len() > 1 {
            self.watchdog.pop_front().unwrap()
        } else {
            self.watchdog.front().unwrap().clone()
        })
    }
    fn live_faults(
        &mut self,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<KernelRead>> {
        (self.live)(after)
    }
}

struct ActiveClock {
    active: Duration,
    wall: Duration,
    sleeps: VecDeque<(Duration, Duration)>,
}

impl ActiveClock {
    fn one() -> Self {
        Self {
            active: Duration::ZERO,
            wall: Duration::ZERO,
            sleeps: [(Duration::from_secs(120), Duration::from_secs(120))].into(),
        }
    }
}

impl Clock for ActiveClock {
    fn now(&self) -> Timestamp {
        Timestamp::UNIX_EPOCH.checked_add(self.wall).unwrap()
    }
    fn monotonic(&self) -> Result<Duration, MachineError> {
        Ok(self.active)
    }
    fn sleep(
        &mut self,
        cancel: &CancellationToken,
        duration: Duration,
    ) -> Result<(), MachineError> {
        assert_eq!(duration, WATCHDOG_POLL);
        let (active, wall) = self.sleeps.pop_front().unwrap();
        self.active += active;
        self.wall += wall;
        if self.sleeps.is_empty() {
            cancel.cancel();
            return Err(MachineError::Cancelled);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
enum FinalFailure {
    Readback,
    CheckpointAppend,
}

#[test_case(FinalFailure::Readback ; "final offset readback")]
#[test_case(FinalFailure::CheckpointAppend ; "final checkpoint append")]
fn a_failure_after_cancellation_records_no_clean_stop_and_the_next_boot_reports_it_unclean(
    failure: FinalFailure,
) {
    let fs = Arc::new(MemFs::new());
    let mut journal = applying::seed_on(Arc::clone(&fs));
    let cancel = CancellationToken::new();
    let mut hardware = match failure {
        FinalFailure::Readback => {
            let mut hardware = applying::Hardware::stock();
            // Preflight, boot start, startup transaction and startup guard read successfully.
            hardware.failure = applying::Failure::Read(5);
            hardware
        }
        FinalFailure::CheckpointAppend => {
            let fs = Arc::clone(&fs);
            let cancel = cancel.clone();
            applying::Hardware::with_reading(Reading::fixed().logging(move |_| {
                if cancel.is_cancelled() {
                    // The stop checkpoint, zero intent, write and readback precede the final checkpoint.
                    fs.inject(Fault::before(Op::Write, 5).named("events.jsonl"));
                }
                Ok(KernelRead::default())
            }))
        }
    };
    let error = serve(
        &mut journal,
        &mut hardware,
        &mut ActiveClock::one(),
        &cancel,
        &current_build(),
    )
    .unwrap_err();
    assert!(cancel.is_cancelled());
    assert!(
        matches!(
            (failure, &error),
            (
                FinalFailure::Readback,
                Error::Observe(MachineError::BootMissing)
            ) | (FinalFailure::CheckpointAppend, Error::Append(_))
        ),
        "{error:?}"
    );
    assert_eq!(hardware.emergency, 1);
    assert_eq!(hardware.current, 0);
    drop(journal);

    let mut next = state_dir(fs)
        .open_at("day-b".to_owned().into(), Timestamp::UNIX_EPOCH)
        .unwrap();
    let expected_checkpoints: &[_] = match failure {
        FinalFailure::Readback => &[0],
        FinalFailure::CheckpointAppend => &[0, 120],
    };
    let checkpoints: Vec<_> = next
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DayCheckpoint(checkpoint) = &event.payload {
                Some((event.seq, checkpoint.applied_secs))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        checkpoints
            .iter()
            .map(|(_, seconds)| *seconds)
            .collect::<Vec<_>>(),
        expected_checkpoints
    );
    let last_checkpoint = checkpoints.last().unwrap().0;
    let expected_tail: &[_] = match failure {
        FinalFailure::Readback => &[],
        FinalFailure::CheckpointAppend => &[
            EventKind::SmuIntent,
            EventKind::SmuWrite,
            EventKind::SmuReadback,
        ],
    };
    assert_eq!(
        next.events()
            .iter()
            .filter(|event| event.seq > last_checkpoint)
            .map(Event::kind)
            .collect::<Vec<_>>(),
        expected_tail
    );
    assert!(
        !next
            .events()
            .iter()
            .any(|event| event.kind() == EventKind::DayCleanStop)
    );
    serve(
        &mut next,
        &mut applying::Hardware::stock(),
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let ends: Vec<_> = next
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DayUncleanEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].boot.as_str(), "day-a");
    assert_eq!(ends[0].last_checkpoint, Some(last_checkpoint));
    assert_eq!(ends[0].offsets, offsets(0));
}

#[test_case(Some(ResetKind::Watchdog), true; "successor reset evidence")]
#[test_case(None, true; "unavailable successor evidence")]
#[test_case(None, false; "previous boot has no checkpoint")]
fn next_boot_records_unclean_end_without_classifying_it(
    reason: Option<ResetKind>,
    checkpointed: bool,
) {
    let mut journal = applying::seed();
    journal
        .append(
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: Some(Duration::ZERO),
            },
            DayBootStart {
                build: current_build(),
                bios: bios(),
                offsets: offsets(-10),
                watchdog: None,
            }
            .into(),
            &[],
        )
        .unwrap();
    let checkpoint = checkpointed.then(|| {
        journal
            .append(
                Stamp {
                    time: Timestamp::UNIX_EPOCH,
                    mono: Some(Duration::from_secs(600)),
                },
                DayCheckpoint {
                    applied_secs: 600,
                    load_mix: LoadMix::default(),
                    watchdog: None,
                }
                .into(),
                &[],
            )
            .unwrap()
            .seq
    });
    journal.reboot("day-b".to_owned().into()).unwrap();
    let mut reading = Reading::fixed();
    reading.reason = reason;
    serve(
        &mut journal,
        &mut applying::Hardware::with_reading(reading),
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let ends: Vec<_> = journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DayUncleanEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].boot.as_str(), "day-a");
    assert_eq!(ends[0].last_checkpoint, checkpoint);
    assert_eq!(ends[0].offsets, offsets(-10));
    assert_eq!(ends[0].reset_reason.kind, reason);
    assert_eq!(ends[0].reset_reason.supported, reason.is_some());
}

#[test]
fn clean_previous_boot_is_not_journaled_as_unclean() {
    let mut journal = applying::seed();
    serve(
        &mut journal,
        &mut applying::Hardware::stock(),
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    journal.reboot("day-b".to_owned().into()).unwrap();
    serve(
        &mut journal,
        &mut applying::Hardware::stock(),
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    assert_eq!(Ledger::of(journal.events()).boots.len(), 2);
    assert!(
        !journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::DayUncleanEnd(_)))
    );
}

#[test]
fn same_boot_restart_preserves_hours_without_counting_downtime() {
    let mut journal = applying::seed();
    serve(
        &mut journal,
        &mut applying::Hardware::stock(),
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let mut clock = ActiveClock::one();
    clock.active = Duration::from_mins(150);
    serve(
        &mut journal,
        &mut applying::Hardware::stock(),
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let ledger = Ledger::of(journal.events());
    assert_eq!(ledger.boots.len(), 1);
    assert_eq!(ledger.boots.values().next().unwrap().seconds, 240);
    assert_eq!(ledger.profiles.values().copied().collect::<Vec<_>>(), [240]);
}

#[test_case(Duration::from_secs(5), Duration::from_secs(5); "sample tick")]
#[test_case(Duration::from_secs(5), Duration::from_secs(125); "first tick after deep sleep")]
#[test_case(Duration::from_secs(600), Duration::from_secs(600); "checkpoint")]
fn a_stopped_timer_at_stock_is_journaled_before_the_next_armed_read(
    active: Duration,
    wall: Duration,
) {
    let stopped = WatchdogState::Unarmed {
        detail: "watchdog0 open but not counting: timeleft 4294967295 s exceeds timeout 60 s"
            .into(),
    };
    let reading = Reading {
        watchdog: [armed(), armed(), stopped.clone(), armed()].into(),
        ..Reading::fixed()
    };
    let mut journal = applying::seed();
    serve(
        &mut journal,
        &mut applying::Hardware::with_reading(reading),
        &mut ticking(2, active, wall),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let checks: Vec<_> = journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DayCheckpoint(checkpoint) = &event.payload {
                Some(checkpoint.watchdog.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        checks,
        [Some(armed()), Some(stopped), Some(armed()), Some(armed())]
    );
    assert_eq!(Ledger::of(journal.events()).watchdog, Some(armed()));
}

/// An active clock that wakes `ticks` times, each after `active` of active time and `wall` of wall time.
fn ticking(ticks: usize, active: Duration, wall: Duration) -> ActiveClock {
    ActiveClock {
        active: Duration::ZERO,
        wall: Duration::ZERO,
        sleeps: std::iter::repeat_n((active, wall), ticks).collect(),
    }
}

fn uncorrected_mce(seconds: u64) -> Mce {
    Mce {
        cpu: CpuId::from(0_u16),
        core: Some(CoreId::new(0).unwrap()),
        bank: 1,
        bank_type: BankType::LoadStore,
        corrected: false,
        time: Timestamp::UNIX_EPOCH,
        monotonic: Duration::from_secs(seconds),
        lines: vec!["[Hardware Error]: Uncorrected".into()],
    }
}

fn trap(kind: ProcessFaultKind, pid: u32, cpu: Option<u16>, seconds: u64) -> ProcessFault {
    ProcessFault {
        pid,
        name: "worker".into(),
        scope: None,
        kind,
        cpu: cpu.map(CpuId::from),
        core: cpu.map(|_| CoreId::new(0).unwrap()),
        monotonic: Duration::from_secs(seconds),
        lines: vec!["trap".into()],
    }
}

fn logged_faults() -> KernelRead {
    KernelRead {
        mces: vec![uncorrected_mce(2)],
        faults: vec![
            trap(ProcessFaultKind::IllegalInstruction, 42, Some(0), 4),
            trap(ProcessFaultKind::Segmentation, 43, Some(0), 5),
        ],
        errors: vec![KernelError {
            kind: KernelErrorKind::SoftLockup,
            monotonic: Duration::from_secs(3),
            lines: vec!["watchdog: BUG: soft lockup - CPU#0 stuck for 26s!".into()],
        }],
        cursor: KernelCursor::from("c1".to_owned()),
    }
}

fn day_faults(journal: &Journal) -> Vec<(&BootId, &DayFault)> {
    journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DayFault(fault) = &event.payload {
                Some((&event.boot, fault))
            } else {
                None
            }
        })
        .collect()
}

fn serve_for(journal: &mut Journal, reading: Reading, ticks: usize) {
    serve(
        journal,
        &mut applying::Hardware::with_reading(reading),
        &mut ticking(ticks, Duration::from_secs(30), Duration::from_secs(30)),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
}

#[test]
fn each_live_fault_is_journaled_once_with_its_boot_and_the_applied_offsets_however_often_it_is_read()
 {
    let afters = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&afters);
    let reading = Reading::fixed().logging(move |after| {
        seen.borrow_mut().push(after.cloned());
        Ok(logged_faults())
    });
    let mut journal = applying::seed();
    serve_for(&mut journal, reading, 3);

    let faults = day_faults(&journal);
    assert_eq!(
        faults
            .iter()
            .map(|(_, fault)| (fault.fault.clone(), fault.monotonic_ns))
            .collect::<Vec<_>>(),
        [
            (
                LiveFault::MachineCheck {
                    cpu: CpuId::from(0_u16),
                    core: Some(CoreId::new(0).unwrap()),
                    bank: 1,
                    bank_type: BankType::LoadStore,
                    corrected: false,
                },
                2_000_000_000
            ),
            (
                LiveFault::KernelError {
                    error: KernelErrorKind::SoftLockup
                },
                3_000_000_000
            ),
            (
                LiveFault::Trap {
                    trap: Trap::IllegalInstruction,
                    name: "worker".into(),
                    pid: 42,
                    cpu: Some(CpuId::from(0_u16)),
                    core: Some(CoreId::new(0).unwrap()),
                },
                4_000_000_000
            ),
        ],
        "a segmentation fault is not recorded"
    );
    for (boot, fault) in &faults {
        assert_eq!(boot.as_str(), "day-a");
        assert_eq!(fault.offsets, offsets(0));
    }
    assert_eq!(
        *afters.borrow(),
        [
            None,
            Some(KernelCursor::from("c1".to_owned())),
            Some(KernelCursor::from("c1".to_owned())),
            Some(KernelCursor::from("c1".to_owned())),
        ],
        "the first read takes the whole boot, the later ones resume after the cursor"
    );
}

#[test]
fn a_restart_within_the_boot_does_not_journal_a_fault_again_but_a_new_boot_does() {
    let mut journal = applying::seed();
    serve_for(
        &mut journal,
        Reading::fixed().logging(|_| Ok(logged_faults())),
        1,
    );
    serve_for(
        &mut journal,
        Reading::fixed().logging(|_| Ok(logged_faults())),
        1,
    );
    assert_eq!(day_faults(&journal).len(), 3);

    journal.reboot("day-b".to_owned().into()).unwrap();
    serve_for(
        &mut journal,
        Reading::fixed().logging(|_| Ok(logged_faults())),
        1,
    );
    let faults = day_faults(&journal);
    assert_eq!(
        faults
            .iter()
            .map(|(boot, _)| boot.as_str())
            .collect::<Vec<_>>(),
        ["day-a", "day-a", "day-a", "day-b", "day-b", "day-b"]
    );
}

#[test]
fn the_kernel_line_and_the_core_dump_of_one_killed_process_are_one_fault() {
    let mut journal = applying::seed();
    let reading = Reading::fixed().logging(|_| {
        Ok(KernelRead {
            faults: vec![
                trap(ProcessFaultKind::Bus, 7, Some(0), 10),
                trap(ProcessFaultKind::Bus, 7, None, 12),
                trap(ProcessFaultKind::Bus, 8, None, 12),
            ],
            ..KernelRead::default()
        })
    });
    serve_for(&mut journal, reading, 1);
    let pids: Vec<_> = day_faults(&journal)
        .iter()
        .map(|(_, fault)| {
            let LiveFault::Trap { pid, .. } = &fault.fault else {
                panic!("{:?}", fault.fault)
            };
            *pid
        })
        .collect();
    assert_eq!(pids, [7, 8]);
}

#[test]
fn an_unresumable_cursor_reads_the_boot_again_without_repeating_a_fault() {
    let afters = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&afters);
    let mut calls = 0;
    let reading = Reading::fixed().logging(move |after| {
        seen.borrow_mut().push(after.cloned());
        calls += 1;
        if calls == 2 {
            return Err(Partial {
                value: KernelRead::default(),
                error: MachineError::CursorMissing,
            });
        }
        Ok(logged_faults())
    });
    let mut journal = applying::seed();
    serve_for(&mut journal, reading, 3);
    assert_eq!(day_faults(&journal).len(), 3);
    assert_eq!(
        *afters.borrow(),
        [
            None,
            Some(KernelCursor::from("c1".to_owned())),
            None,
            Some(KernelCursor::from("c1".to_owned())),
            Some(KernelCursor::from("c1".to_owned())),
        ],
        "the lost cursor is replaced by a read of the boot within the same poll"
    );
}

#[test]
fn a_cursor_lost_at_the_stop_is_replaced_before_the_clean_stop_so_no_later_fault_is_lost() {
    let mut calls = 0;
    let reading = Reading::fixed().logging(move |after| {
        calls += 1;
        match (calls, after) {
            (1, None) => Ok(logged_faults()),
            (2, Some(_)) => Err(Partial {
                value: KernelRead::default(),
                error: MachineError::CursorMissing,
            }),
            (3, None) => {
                let mut read = logged_faults();
                read.mces.push(uncorrected_mce(9));
                Ok(read)
            }
            other => panic!("{other:?}"),
        }
    });
    let mut journal = applying::seed();
    serve_for(&mut journal, reading, 1);
    let kinds: Vec<_> = journal
        .events()
        .iter()
        .map(|event| event.payload.kind())
        .filter(|kind| matches!(kind, EventKind::DayFault | EventKind::DayCleanStop))
        .collect();
    assert_eq!(
        kinds,
        [
            EventKind::DayFault,
            EventKind::DayFault,
            EventKind::DayFault,
            EventKind::DayFault,
            EventKind::DayCleanStop,
        ]
    );
}

#[test]
fn a_later_crash_that_reuses_the_pid_is_another_fault() {
    let mut journal = applying::seed();
    let crash = |seconds: u64| {
        move |_: Option<&KernelCursor>| {
            Ok(KernelRead {
                faults: vec![
                    trap(ProcessFaultKind::IllegalInstruction, 7, Some(0), 10),
                    trap(ProcessFaultKind::IllegalInstruction, 7, None, seconds),
                ],
                ..KernelRead::default()
            })
        }
    };
    serve_for(&mut journal, Reading::fixed().logging(crash(400)), 1);
    assert_eq!(
        day_faults(&journal).len(),
        2,
        "400 s later is a second death"
    );
    serve_for(&mut journal, Reading::fixed().logging(crash(400)), 1);
    assert_eq!(
        day_faults(&journal).len(),
        2,
        "a restart records neither again"
    );
}

#[test]
fn a_general_protection_fault_is_journaled_with_its_trap_boot_and_offsets() {
    let mut journal = applying::seed();
    let reading = Reading::fixed().logging(|_| {
        Ok(KernelRead {
            faults: vec![trap(ProcessFaultKind::GeneralProtection, 9, Some(0), 5)],
            ..KernelRead::default()
        })
    });
    serve_for(&mut journal, reading, 1);
    let faults = day_faults(&journal);
    assert_eq!(faults.len(), 1);
    let (boot, fault) = faults[0];
    assert_eq!(boot.as_str(), "day-a");
    assert_eq!(fault.offsets, offsets(0));
    assert!(matches!(
        fault.fault,
        LiveFault::Trap {
            trap: Trap::GeneralProtection,
            pid: 9,
            ..
        }
    ));
}

#[test]
fn a_failed_log_read_keeps_the_first_error_and_never_writes_a_clean_stop() {
    let reading = Reading::fixed().logging(|_| {
        Err(Partial {
            value: KernelRead {
                mces: vec![uncorrected_mce(2)],
                ..KernelRead::default()
            },
            error: MachineError::BootMissing,
        })
    });
    let mut journal = applying::seed();
    let mut hardware = applying::Hardware::with_reading(reading);
    let error = serve(
        &mut journal,
        &mut hardware,
        &mut ticking(1, Duration::from_secs(30), Duration::from_secs(30)),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap_err();
    assert!(
        matches!(error, Error::Observe(MachineError::BootMissing)),
        "{error:?}"
    );
    assert_eq!(day_faults(&journal).len(), 1);
    assert_eq!(hardware.emergency, 1);
    assert_eq!(hardware.current, 0);
    assert!(
        !journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::DayCleanStop(_)))
    );
}
