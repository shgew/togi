use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use test_case::test_case;
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

fn journal() -> Journal {
    #[expect(
        clippy::disallowed_methods,
        reason = "observer lifecycle tests inject an in-memory journal"
    )]
    let dir = StateDir::lock_with_fs(Arc::new(MemFs::new()), Path::new("/state")).unwrap();
    let mut journal = dir
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
}

impl Reading {
    fn fixed() -> Self {
        Self {
            offsets: [offsets(-10)].into(),
            reason: None,
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
