use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use test_case::test_case;
use togi_journal::fs::{Fault, Op};
use togi_journal::{MemFs, SessionId, SessionStart, StateDir};
use togi_machine::{BiosContext, CcdId, CoreInfo, CpuId, Error as MachineError, ResetKind};

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

struct Reading {
    offsets: VecDeque<PerCore<ObservedOffset>>,
    reason: Option<ResetKind>,
    watchdog: WatchdogState,
}

fn armed() -> WatchdogState {
    WatchdogState::Armed {
        detail: "watchdog0 (SP5100 TCO timer) active, held by PID 1".into(),
    }
}

impl Reading {
    fn fixed() -> Self {
        Self {
            offsets: [offsets(-10)].into(),
            reason: None,
            watchdog: armed(),
        }
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
    fn watchdog(&mut self) -> WatchdogState {
        self.watchdog.clone()
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
    fn watchdog(&mut self) -> WatchdogState {
        self.reading.watchdog()
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
        assert_eq!(duration, CHECKPOINT);
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
                watchdog: None,
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
            DayCheckpoint { applied_secs: 600 }.into(),
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
        reason: None,
        watchdog: armed(),
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

#[test_case(armed() ; "armed")]
#[test_case(WatchdogState::Unarmed { detail: "watchdog0 state \"inactive\", want active".into() } ; "unarmed")]
fn the_observer_records_whether_pid_1_s_watchdog_is_armed_as_it_starts(state: WatchdogState) {
    let mut journal = journal();
    let mut reading = Reading {
        watchdog: state.clone(),
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
    let first = events
        .iter()
        .find(|event| event.kind() == togi_journal::EventKind::DayBootStart)
        .unwrap();
    let Payload::DayBootStart(start) = &first.payload else {
        panic!("a day.boot_start event carries a DayBootStart payload");
    };
    assert_eq!(start.watchdog, Some(state.clone()));
    assert_eq!(Ledger::of(journal.events()).watchdog, Some(state));
}
