use test_case::test_case;
use togi_journal::{
    ApplicationId, ApplyCause, CcdVector, DayApplied, DayStep, DecisionId, SessionBaseline,
    SessionContext, StepCauses, StepKind,
};
use togi_machine::{CcdProfile, Smu, SmuValidated, Stopped, TargetOffset};

use super::*;

#[derive(Clone, Copy)]
enum Failure {
    None,
    Write(usize),
    Read(usize),
}

struct Hardware {
    reading: Reading,
    current: i64,
    writes: Vec<TargetOffset>,
    read_count: usize,
    emergency: usize,
    failure: Failure,
    firmware: BiosContext,
}

impl Hardware {
    fn stock() -> Self {
        Self {
            reading: Reading::fixed(),
            current: 0,
            writes: Vec::new(),
            read_count: 0,
            emergency: 0,
            failure: Failure::None,
            firmware: bios(),
        }
    }
}

impl Smu for Hardware {
    fn offset(&mut self, _: CoreId) -> Result<ObservedOffset, MachineError> {
        self.read_count += 1;
        if matches!(self.failure, Failure::Read(at) if at == self.read_count) {
            return Err(MachineError::BootMissing);
        }
        Ok(ObservedOffset(self.current))
    }
    fn set_offset(&mut self, _: CoreId, _: TargetOffset) -> Result<(), MachineError> {
        panic!("day must use the all-core stock transaction")
    }
    fn set_all_offsets(&mut self, target: TargetOffset) -> Result<(), MachineError> {
        self.writes.push(target);
        assert_eq!(target, TargetOffset::ZERO);
        if matches!(self.failure, Failure::Write(at) if at == self.writes.len()) {
            return Err(MachineError::BootMissing);
        }
        self.current = target.get();
        Ok(())
    }
    fn emergency_zero(&mut self, _: SmuValidated, _: &Stopped) -> Result<(), MachineError> {
        self.emergency += 1;
        self.current = 0;
        Ok(())
    }
}

impl Observer for Hardware {
    #[expect(
        clippy::disallowed_methods,
        reason = "in-memory day hardware validates its fake SMU"
    )]
    fn validated(&mut self) -> Result<SmuValidated, MachineError> {
        Ok(SmuValidated::checked())
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "in-memory day hardware has no workload process"
    )]
    fn stop_workloads(&mut self) -> Result<Stopped, MachineError> {
        Ok(Stopped::confirmed())
    }
    fn bios_context(&mut self) -> Result<BiosContext, MachineError> {
        Ok(self.firmware.clone())
    }
    fn offsets(&mut self) -> Result<PerCore<ObservedOffset>, MachineError> {
        Ok([(
            CoreId::new(0).unwrap(),
            self.offset(CoreId::new(0).unwrap())?,
        )]
        .into_iter()
        .collect())
    }
    fn watchdog(&mut self) -> WatchdogState {
        self.reading.watchdog()
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

fn seed_on(fs: Arc<MemFs>) -> Journal {
    let mut journal = journal_on(fs);
    append(
        &mut journal,
        &ActiveClock::one(),
        SessionContext::from(bios()),
    )
    .unwrap();
    append(
        &mut journal,
        &ActiveClock::one(),
        SessionBaseline {
            offsets: offsets(0),
        },
    )
    .unwrap();
    journal
}

fn seed() -> Journal {
    seed_on(Arc::new(MemFs::new()))
}

fn target(journal: &Journal, value: i64) -> CcdVector {
    let topology = session_cores(journal.events()).unwrap();
    CcdVector::expand(
        CcdProfile::uniform([CcdId::new(0).unwrap()], TargetOffset::new(value).unwrap()).unwrap(),
        topology,
    )
    .unwrap()
}

fn proposed(journal: &mut Journal) -> Seq {
    let zero = target(journal, 0);
    let nonzero = target(journal, -5);
    journal
        .append(
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: Some(Duration::ZERO),
            },
            DayStep {
                decision: DecisionId(0),
                cold_boot: journal.boot().clone(),
                old: zero,
                proposed: nonzero,
                step: StepKind::Promote {
                    changed: CcdId::new(0).unwrap(),
                },
                predecessor: None,
                hours: Vec::new(),
                hold_s: 0,
                causes: StepCauses::default(),
                reason: "qualified historical proposal".into(),
            }
            .into(),
            &[],
        )
        .unwrap()
        .seq
}

fn applications(journal: &Journal) -> Vec<&DayApplied> {
    journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DayApplied(value) = &event.payload {
                Some(value)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn cold_boot_applies_only_stock_and_stop_verifies_before_clean_marker() {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    serve(
        &mut journal,
        &mut hardware,
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    assert_eq!(hardware.writes, [TargetOffset::ZERO, TargetOffset::ZERO]);
    assert_eq!(hardware.emergency, 0);
    assert_eq!(applications(&journal).len(), 1);
    assert_eq!(applications(&journal)[0].target, target(&journal, 0));
    let kinds: Vec<_> = journal.events().iter().map(Event::kind).collect();
    assert_eq!(
        &kinds[kinds.len() - 3..],
        [
            EventKind::SmuReadback,
            EventKind::DayCheckpoint,
            EventKind::DayCleanStop
        ]
    );
}

#[test]
fn a_nonzero_proposal_commits_only_stock_as_a_reset_without_consuming_the_promotion() {
    let mut journal = seed();
    let proposal = proposed(&mut journal);
    let mut hardware = Hardware::stock();
    serve(
        &mut journal,
        &mut hardware,
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let applied = applications(&journal)[0];
    assert_eq!(applied.cause, ApplyCause::Reset { cause: proposal });
    assert_eq!(applied.target, target(&journal, 0));
    assert_eq!(applied.readbacks, offsets(0));
    let mut policy = togi_tuner::policy::State::new(togi_tuner::policy::Params::SPEC);
    for event in journal.events() {
        policy.fold(event).unwrap();
    }
    assert_eq!(
        policy.project().applied,
        Some(target(&journal, 0).ccd().clone())
    );
    assert_eq!(
        policy.project().day.last_step.unwrap().1.proposed,
        target(&journal, -5)
    );
    let event = journal
        .events()
        .iter()
        .find(|event| matches!(event.payload, Payload::DayApplied(_)))
        .unwrap();
    assert!(
        event
            .msg
            .contains("nonzero day offsets are not enabled yet")
    );
    assert!(event.msg.contains("no promotion consumed"));
}

#[test]
fn nonzero_baseline_is_refused_without_a_write_or_application() {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    hardware.current = -5;
    assert!(matches!(
        serve(
            &mut journal,
            &mut hardware,
            &mut ActiveClock::one(),
            &CancellationToken::new(),
            &current_build()
        ),
        Err(Error::Baseline)
    ));
    assert_eq!(hardware.writes, []);
    assert_eq!(hardware.emergency, 0);
    assert_eq!(applications(&journal).len(), 0);
}

#[test_case("unarmed"; "unarmed watchdog")]
#[test_case("active timer did not count down"; "non-counting watchdog")]
fn an_unarmed_or_noncounting_watchdog_refuses_without_a_write(detail: &str) {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    hardware.reading.watchdog = [WatchdogState::Unarmed {
        detail: detail.into(),
    }]
    .into();
    assert!(matches!(
        serve(
            &mut journal,
            &mut hardware,
            &mut ActiveClock::one(),
            &CancellationToken::new(),
            &current_build()
        ),
        Err(Error::Watchdog)
    ));
    assert_eq!(hardware.writes, []);
    assert_eq!(hardware.emergency, 0);
}

#[test]
fn a_defect_stop_refuses_a_nonzero_proposal_and_takes_only_emergency_zero() {
    let mut journal = seed();
    proposed(&mut journal);
    append(
        &mut journal,
        &ActiveClock::one(),
        togi_journal::DeadEnd {
            condition: togi_journal::DeadEndCondition::Defect,
            core: None,
            detail: "unresolved too-aggressive finding".into(),
            action: None,
        },
    )
    .unwrap();
    let mut hardware = Hardware::stock();
    assert!(matches!(
        serve(
            &mut journal,
            &mut hardware,
            &mut ActiveClock::one(),
            &CancellationToken::new(),
            &current_build()
        ),
        Err(Error::Defect)
    ));
    assert_eq!(hardware.writes, []);
    assert_eq!(hardware.emergency, 1);
    assert_eq!(applications(&journal).len(), 0);
    assert!(
        !journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::DayCleanStop(_)))
    );
}

#[test]
fn same_boot_restart_reapplies_without_another_step() {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    let mut clock = ActiveClock::one();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let steps = journal
        .events()
        .iter()
        .filter(|event| matches!(event.payload, Payload::DayStep(_)))
        .count();
    clock.sleeps = [(Duration::from_secs(5), Duration::from_secs(5))].into();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    assert_eq!(
        journal
            .events()
            .iter()
            .filter(|event| matches!(event.payload, Payload::DayStep(_)))
            .count(),
        steps
    );
    assert_eq!(
        applications(&journal).last().unwrap().cause,
        ApplyCause::Reapplication {}
    );
    assert_eq!(hardware.writes, [TargetOffset::ZERO; 4]);
}

#[test]
fn resume_reapplies_with_verified_readback_and_preserves_the_step() {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    let mut clock = ActiveClock::one();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let steps = journal
        .events()
        .iter()
        .filter(|event| matches!(event.payload, Payload::DayStep(_)))
        .count();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    live.reapply(&mut journal, &mut hardware, &clock).unwrap();
    assert_eq!(applications(&journal).last().unwrap().readbacks, offsets(0));
    assert_eq!(
        applications(&journal).last().unwrap().cause,
        ApplyCause::Reapplication {}
    );
    assert_eq!(
        journal
            .events()
            .iter()
            .filter(|event| matches!(event.payload, Payload::DayStep(_)))
            .count(),
        steps
    );
}

#[test_case("unarmed"; "unarmed watchdog")]
#[test_case("active timer did not count down"; "non-counting watchdog")]
fn resume_with_an_unarmed_or_noncounting_watchdog_refuses_without_a_write(detail: &str) {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    let clock = ActiveClock::one();
    let mut live = crate::day::apply::Live::preflight(&journal, &mut hardware).unwrap();
    live.start(&mut journal, &mut hardware, &clock, false)
        .unwrap();
    let events = journal.events().len();
    let applied = applications(&journal).len();
    let writes = hardware.writes.clone();
    assert!(!writes.is_empty());
    hardware.reading.watchdog = [WatchdogState::Unarmed {
        detail: detail.into(),
    }]
    .into();
    assert!(matches!(
        live.reapply(&mut journal, &mut hardware, &clock),
        Err(Error::Watchdog)
    ));
    assert_eq!(hardware.writes, writes);
    assert_eq!(hardware.emergency, 0);
    assert_eq!(applications(&journal).len(), applied);
    // No SMU intent, day application or any other event follows the refusal.
    assert_eq!(journal.events().len(), events);
}

#[test]
fn firmware_change_resets_a_historical_nonzero_profile_before_applying_stock() {
    let mut journal = seed();
    let proposal = proposed(&mut journal);
    let historical = target(&journal, -5);
    let boot = journal.boot().clone();
    journal
        .append(
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: Some(Duration::ZERO),
            },
            DayApplied {
                application: ApplicationId(0),
                cause: ApplyCause::Step {
                    decision: DecisionId(0),
                },
                target: historical,
                readbacks: offsets(-5),
                transactions: Vec::new(),
                boot,
                mono_ns: 0,
            }
            .into(),
            &[proposal],
        )
        .unwrap();
    let mut hardware = Hardware::stock();
    hardware.firmware.microcode = "new".into();
    serve(
        &mut journal,
        &mut hardware,
        &mut ActiveClock::one(),
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let reset = journal
        .events()
        .iter()
        .rev()
        .find_map(|event| {
            if let Payload::DayStep(step) = &event.payload {
                Some(step)
            } else {
                None
            }
        })
        .unwrap();
    assert!(matches!(reset.step, StepKind::FirmwareZero { .. }));
    let actual = applications(&journal).last().copied().unwrap();
    assert_eq!(actual.target, target(&journal, 0));
    assert_eq!(
        actual.cause,
        ApplyCause::Step {
            decision: reset.decision
        }
    );
    assert_eq!(hardware.writes, [TargetOffset::ZERO, TargetOffset::ZERO]);
}

#[test]
fn firmware_change_reapplies_stock_without_inventing_a_step_when_already_stock() {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    let mut clock = ActiveClock::one();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    let steps = journal
        .events()
        .iter()
        .filter(|event| matches!(event.payload, Payload::DayStep(_)))
        .count();
    hardware.firmware.microcode = "new".into();
    clock.sleeps = [(Duration::from_secs(5), Duration::from_secs(5))].into();
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    assert_eq!(
        journal
            .events()
            .iter()
            .filter(|event| matches!(event.payload, Payload::DayStep(_)))
            .count(),
        steps,
        "the tuner's firmware-zero decision is unnecessary when D is already stock"
    );
    assert_eq!(
        applications(&journal).last().unwrap().target,
        target(&journal, 0)
    );
    assert_eq!(
        applications(&journal).last().unwrap().cause,
        ApplyCause::Reapplication {}
    );
}

#[test_case(Failure::Write(1); "failed initial write")]
#[test_case(Failure::Read(3); "failed initial readback")]
#[test_case(Failure::Write(2); "failed stop zero")]
#[test_case(Failure::Read(4); "failed stop readback")]
fn failed_writes_or_readbacks_never_earn_a_clean_marker(failure: Failure) {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    hardware.failure = failure;
    assert!(
        serve(
            &mut journal,
            &mut hardware,
            &mut ActiveClock::one(),
            &CancellationToken::new(),
            &current_build()
        )
        .is_err()
    );
    assert_eq!(hardware.emergency, 1);
    assert!(
        !journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::DayCleanStop(_)))
    );
}

#[test]
fn journal_failure_takes_the_zero_only_emergency_without_a_clean_marker() {
    let fs = Arc::new(MemFs::new());
    let mut journal = seed_on(Arc::clone(&fs));
    fs.inject(Fault::before(Op::Write, 1).named("events.jsonl"));
    let mut hardware = Hardware::stock();
    assert!(
        serve(
            &mut journal,
            &mut hardware,
            &mut ActiveClock::one(),
            &CancellationToken::new(),
            &current_build()
        )
        .is_err()
    );
    assert_eq!(hardware.writes, []);
    assert_eq!(hardware.emergency, 1);
    assert!(
        !journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::DayCleanStop(_)))
    );
}

#[test]
fn stock_applied_hours_exclude_suspended_wall_time() {
    let mut journal = seed();
    let mut hardware = Hardware::stock();
    let mut clock = ActiveClock {
        active: Duration::ZERO,
        wall: Duration::ZERO,
        sleeps: [
            (Duration::from_secs(600), Duration::from_secs(600)),
            (Duration::from_secs(120), Duration::from_mins(62)),
        ]
        .into(),
    };
    serve(
        &mut journal,
        &mut hardware,
        &mut clock,
        &CancellationToken::new(),
        &current_build(),
    )
    .unwrap();
    assert_eq!(
        Ledger::of(journal.events())
            .profiles
            .values()
            .copied()
            .collect::<Vec<_>>(),
        [720]
    );
    let mut policy = togi_tuner::policy::State::new(togi_tuner::policy::Params::SPEC);
    for event in journal.events() {
        policy.fold(event).unwrap();
    }
    let query = togi_tuner::policy::DayQuery {
        boot: journal.boot().clone(),
        invocation: togi_tuner::day::Invocation::SameBoot,
        now: clock.now(),
    };
    loop {
        let owed = policy.day(&query).unwrap();
        if owed.is_empty() {
            break;
        }
        for decision in owed {
            let event = journal
                .append(
                    Stamp {
                        time: clock.now(),
                        mono: Some(clock.monotonic().unwrap()),
                    },
                    decision.payload.into(),
                    &decision.causes,
                )
                .unwrap();
            policy.fold(event).unwrap();
        }
    }
    assert!(
        policy.project().day.hold.is_none(),
        "stock has no nonzero hold; its raw exposure above is still only the 720 active seconds"
    );
}
