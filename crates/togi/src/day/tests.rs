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
    BankType, BiosContext, CcdId, CoreInfo, CpuId, CpuTime, Error as MachineError, KernelCursor,
    KernelError, KernelErrorKind, KernelRead, Mce, Partial, ProcessFault, ProcessFaultKind,
    ResetKind,
};

use super::*;

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

type LiveLog = dyn FnMut(Option<&KernelCursor>) -> Result<KernelRead, Partial<KernelRead>>;

struct Reading {
    offsets: VecDeque<PerCore<ObservedOffset>>,
    reason: Option<ResetKind>,
    times: VecDeque<Vec<CpuTime>>,
    live: Box<LiveLog>,
}

impl Reading {
    fn fixed() -> Self {
        Self {
            offsets: [offsets(-10)].into(),
            reason: None,
            times: [vec![CpuTime {
                cpu: CpuId::from(0_u16),
                busy: 0,
                total: 0,
            }]]
            .into(),
            live: Box::new(|_| Ok(KernelRead::default())),
        }
    }

    fn logging(
        mut self,
        live: impl FnMut(Option<&KernelCursor>) -> Result<KernelRead, Partial<KernelRead>> + 'static,
    ) -> Self {
        self.live = Box::new(live);
        self
    }
}

impl Observer for Reading {
    fn bios_context(&mut self) -> Result<BiosContext, MachineError> {
        Ok(bios())
    }
    fn offsets(&mut self) -> Result<PerCore<ObservedOffset>, MachineError> {
        Ok(if self.offsets.len() > 1 {
            self.offsets.pop_front().unwrap()
        } else {
            *self.offsets.front().unwrap()
        })
    }
    fn reset_reason_after(&mut self, _: &BootId) -> Result<ResetReason, MachineError> {
        self.reason
            .map(|kind| ResetReason {
                kind: Some(kind),
                raw: vec!["reset evidence".into()],
                supported: true,
            })
            .ok_or(MachineError::BootMissing)
    }
    fn end_evidence(&mut self, _: &BootId) -> togi_machine::EndEvidence {
        togi_machine::EndEvidence::unknown()
    }
    fn live_faults(
        &mut self,
        _: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<KernelRead>> {
        (self.live)(after)
    }
    fn cpu_times(&mut self) -> Result<Vec<CpuTime>, MachineError> {
        Ok(if self.times.len() > 1 {
            self.times.pop_front().unwrap()
        } else {
            self.times.front().unwrap().clone()
        })
    }
}

/// Reads like `reading` until `reads` readings succeeded, then fails every read.
struct FailingAfter {
    reading: Reading,
    reads: usize,
}

impl Observer for FailingAfter {
    fn bios_context(&mut self) -> Result<BiosContext, MachineError> {
        self.reading.bios_context()
    }
    fn offsets(&mut self) -> Result<PerCore<ObservedOffset>, MachineError> {
        if self.reads == 0 {
            return Err(MachineError::BootMissing);
        }
        self.reads -= 1;
        self.reading.offsets()
    }
    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, MachineError> {
        self.reading.reset_reason_after(boot)
    }
    fn end_evidence(&mut self, boot: &BootId) -> togi_machine::EndEvidence {
        self.reading.end_evidence(boot)
    }
    fn live_faults(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<KernelRead>> {
        self.reading.live_faults(boot, after)
    }
    fn cpu_times(&mut self) -> Result<Vec<CpuTime>, MachineError> {
        self.reading.cpu_times()
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
        assert_eq!(duration, SAMPLE);
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

#[test]
fn checkpoints_exclude_suspend_and_clean_marker_follows_final_checkpoint() {
    let mut journal = journal();
    let mut clock = ActiveClock {
        active: Duration::ZERO,
        wall: Duration::ZERO,
        sleeps: [
            (Duration::from_secs(600), Duration::from_secs(600)),
            (Duration::from_secs(120), Duration::from_mins(62)),
        ]
        .into(),
    };
    observe(
        &mut journal,
        &mut Reading::fixed(),
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let events = journal.events();
    assert_eq!(
        events
            .iter()
            .filter_map(|event| if let Payload::DayCheckpoint(p) = &event.payload {
                Some(p.applied_secs)
            } else {
                None
            })
            .collect::<Vec<_>>(),
        [600, 720]
    );
    assert_eq!(
        events[events.len() - 2].kind(),
        togi_journal::EventKind::DayCheckpoint
    );
    assert_eq!(
        events.last().unwrap().kind(),
        togi_journal::EventKind::DayCleanStop
    );
    assert_eq!(
        Ledger::of(events)
            .profiles
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [720]
    );
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
    let mut journal = journal_on(fs.clone());
    let mut observer = FailingAfter {
        reading: Reading::fixed(),
        reads: match failure {
            // The boot-start read succeeds; the readback after cancellation fails.
            FinalFailure::Readback => 1,
            FinalFailure::CheckpointAppend => usize::MAX,
        },
    };
    if let FinalFailure::CheckpointAppend = failure {
        // The day.boot_start line is the first write after injection, the final checkpoint the second.
        fs.inject(Fault::before(Op::Write, 2).named("events.jsonl"));
    }
    let cancel = CancellationToken::new();
    let error = observe(
        &mut journal,
        &mut observer,
        &mut ActiveClock::one(),
        &cancel,
        &current_build(),
    )
    .unwrap_err();
    assert!(cancel.is_cancelled());
    assert!(
        matches!(
            (failure, &error),
            (FinalFailure::Readback, Error::Observe(_))
                | (FinalFailure::CheckpointAppend, Error::Append(_))
        ),
        "{error:?}"
    );
    drop(journal);

    let mut next = state_dir(fs)
        .open_at("day-b".to_owned().into(), Timestamp::UNIX_EPOCH)
        .unwrap();
    assert_eq!(
        next.events()
            .iter()
            .map(togi_journal::Event::kind)
            .collect::<Vec<_>>(),
        [
            togi_journal::EventKind::SessionStart,
            togi_journal::EventKind::DayBootStart
        ]
    );
    observe(
        &mut next,
        &mut Reading::fixed(),
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
    assert_eq!(ends[0].last_checkpoint, None);
    assert_eq!(ends[0].offsets, offsets(-10));
}

#[test_case(Some(ResetKind::Watchdog) ; "successor reset evidence")]
#[test_case(None ; "unavailable successor evidence")]
fn next_boot_records_unclean_end_without_classifying_it(reason: Option<ResetKind>) {
    let mut journal = journal();
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
            }
            .into(),
            &[],
        )
        .unwrap();
    let checkpoint = journal
        .append(
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: Some(Duration::from_secs(600)),
            },
            DayCheckpoint {
                applied_secs: 600,
                load_mix: LoadMix::default(),
            }
            .into(),
            &[],
        )
        .unwrap()
        .seq;
    journal.reboot("day-b".to_owned().into()).unwrap();
    let mut reading = Reading::fixed();
    reading.reason = reason;
    observe(
        &mut journal,
        &mut reading,
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
    assert_eq!(ends[0].last_checkpoint, Some(checkpoint));
    assert_eq!(ends[0].offsets, offsets(-10));
    assert_eq!(ends[0].reset_reason.kind, reason);
    assert_eq!(ends[0].reset_reason.supported, reason.is_some());
}

#[test]
fn clean_previous_boot_is_not_journaled_as_unclean() {
    let mut journal = journal();
    observe(
        &mut journal,
        &mut Reading::fixed(),
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    journal.reboot("day-b".to_owned().into()).unwrap();
    observe(
        &mut journal,
        &mut Reading::fixed(),
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
    let mut journal = journal();
    observe(
        &mut journal,
        &mut Reading::fixed(),
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let mut clock = ActiveClock::one();
    clock.active = Duration::from_mins(150);
    observe(
        &mut journal,
        &mut Reading::fixed(),
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

#[test]
fn changed_readback_records_change_before_checkpoint_and_excludes_ambiguous_profile_hours() {
    let mut journal = journal();
    let mut reading = Reading {
        offsets: [offsets(-10), offsets(-5)].into(),
        ..Reading::fixed()
    };
    observe(
        &mut journal,
        &mut reading,
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let events = journal.events();
    assert_eq!(
        events[events.len() - 3].kind(),
        togi_journal::EventKind::DayOffsetsChanged
    );
    let ledger = Ledger::of(events);
    assert_eq!(ledger.boots.values().next().unwrap().seconds, 120);
    assert!(ledger.profiles.is_empty());
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

fn observe_for(journal: &mut Journal, reading: &mut Reading, ticks: usize) {
    observe(
        journal,
        reading,
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
    let mut reading = Reading::fixed().logging(move |after| {
        seen.borrow_mut().push(after.cloned());
        Ok(logged_faults())
    });
    let mut journal = journal();
    observe_for(&mut journal, &mut reading, 3);

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
        assert_eq!(fault.offsets, offsets(-10));
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
    let mut journal = journal();
    observe_for(
        &mut journal,
        &mut Reading::fixed().logging(|_| Ok(logged_faults())),
        1,
    );
    observe_for(
        &mut journal,
        &mut Reading::fixed().logging(|_| Ok(logged_faults())),
        1,
    );
    assert_eq!(day_faults(&journal).len(), 3);

    journal.reboot("day-b".to_owned().into()).unwrap();
    observe_for(
        &mut journal,
        &mut Reading::fixed().logging(|_| Ok(logged_faults())),
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
    let mut journal = journal();
    let mut reading = Reading::fixed().logging(|_| {
        Ok(KernelRead {
            faults: vec![
                trap(ProcessFaultKind::Bus, 7, Some(0), 10),
                trap(ProcessFaultKind::Bus, 7, None, 12),
                trap(ProcessFaultKind::Bus, 8, None, 12),
            ],
            ..KernelRead::default()
        })
    });
    observe_for(&mut journal, &mut reading, 1);
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
    let mut reading = Reading::fixed().logging(move |after| {
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
    let mut journal = journal();
    observe_for(&mut journal, &mut reading, 3);
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
    let mut reading = Reading::fixed().logging(move |after| {
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
    let mut journal = journal();
    observe_for(&mut journal, &mut reading, 1);
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
    let mut journal = journal();
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
    observe_for(&mut journal, &mut Reading::fixed().logging(crash(400)), 1);
    assert_eq!(
        day_faults(&journal).len(),
        2,
        "400 s later is a second death"
    );
    observe_for(&mut journal, &mut Reading::fixed().logging(crash(400)), 1);
    assert_eq!(
        day_faults(&journal).len(),
        2,
        "a restart records neither again"
    );
}

#[test]
fn a_general_protection_fault_is_journaled_with_its_trap_boot_and_offsets() {
    let mut journal = journal();
    let mut reading = Reading::fixed().logging(|_| {
        Ok(KernelRead {
            faults: vec![trap(ProcessFaultKind::GeneralProtection, 9, Some(0), 5)],
            ..KernelRead::default()
        })
    });
    observe_for(&mut journal, &mut reading, 1);
    let faults = day_faults(&journal);
    assert_eq!(faults.len(), 1);
    let (boot, fault) = faults[0];
    assert_eq!(boot.as_str(), "day-a");
    assert_eq!(fault.offsets, offsets(-10));
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
fn a_failed_log_read_journals_what_it_read_and_ends_the_observer_without_a_clean_stop() {
    let mut reading = Reading::fixed().logging(|_| {
        Err(Partial {
            value: KernelRead {
                mces: vec![uncorrected_mce(2)],
                ..KernelRead::default()
            },
            error: MachineError::BootMissing,
        })
    });
    let mut journal = journal();
    let error = observe(
        &mut journal,
        &mut reading,
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
    assert!(
        !journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::DayCleanStop(_)))
    );
}

fn busy_ticks(step: u64, busy: u64) -> Vec<CpuTime> {
    vec![CpuTime {
        cpu: CpuId::from(0_u16),
        busy: busy * step,
        total: 100 * step,
    }]
}

#[test]
fn the_checkpoint_carries_the_load_mix_of_the_active_time_and_a_suspended_interval_adds_nothing() {
    let mut reading = Reading {
        times: [
            busy_ticks(0, 0),
            busy_ticks(1, 100),
            busy_ticks(2, 100),
            busy_ticks(3, 100),
        ]
        .into(),
        ..Reading::fixed()
    };
    let mut clock = ActiveClock {
        active: Duration::ZERO,
        wall: Duration::ZERO,
        sleeps: [
            (SAMPLE, SAMPLE),
            (SAMPLE, SAMPLE),
            // The machine slept for an hour of wall time; no active time passed, so nothing is sampled.
            (Duration::ZERO, Duration::from_hours(1)),
            (SAMPLE, SAMPLE),
        ]
        .into(),
    };
    let mut journal = journal();
    observe(
        &mut journal,
        &mut reading,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let Some(Payload::DayCheckpoint(last)) = journal
        .events()
        .iter()
        .rev()
        .map(|event| &event.payload)
        .find(|payload| matches!(payload, Payload::DayCheckpoint(_)))
    else {
        panic!("no checkpoint")
    };
    assert_eq!(last.applied_secs, 15);
    assert_eq!(
        last.load_mix,
        LoadMix {
            all_threads_ms: 15_000,
            ..LoadMix::default()
        }
    );
    assert_eq!(
        Ledger::of(journal.events())
            .boots
            .values()
            .next()
            .unwrap()
            .load_mix,
        last.load_mix
    );
}

#[test]
fn a_restart_within_the_boot_continues_the_recorded_load_mix() {
    let mut journal = journal();
    for _ in 0..2 {
        let mut reading = Reading {
            times: [busy_ticks(0, 0), busy_ticks(1, 100)].into(),
            ..Reading::fixed()
        };
        observe(
            &mut journal,
            &mut reading,
            &mut ticking(1, SAMPLE, SAMPLE),
            &CancellationToken::new(),
            &current_build(),
        )
        .unwrap();
    }
    assert_eq!(
        Ledger::of(journal.events())
            .boots
            .values()
            .next()
            .unwrap()
            .load_mix
            .all_threads_ms,
        10_000
    );
}
