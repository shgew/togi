//! The day query through the journal: what a boot owes once it has a step, which live machine checks are
//! CPU evidence, and how a failed qualification holds its proposal.

use test_case::test_case;
use togi_journal::{
    Activity, Classification, DayFault, HoldReason, LiveFault, Payload, QualificationPurpose,
    QualificationState, StepKind,
};
use togi_machine::{BankType, BootId, CcdProfile, CoreId, CpuId};
use togi_tuner::day::Invocation;
use togi_tuner::policy::DayQuery;

use crate::fixture::{Ending, Journal, at, firmware, offsets_of};
use crate::flow::{cold_boot, qualifications, steps};

fn same_boot(boot: &str, second: i64) -> DayQuery {
    DayQuery {
        boot: BootId::from(boot.to_owned()),
        invocation: Invocation::SameBoot,
        now: at(second),
    }
}

#[test]
fn a_firmware_change_later_in_the_boot_still_zeroes_d_on_a_same_boot_query() {
    let (mut journal, _) = Journal::promoted_compact();
    // The boot `day1` already has its promotion; the firmware context changes before a service restart.
    journal.append(firmware("2").into(), &[]);
    let committed = journal.settle_day(&same_boot("day1", 60_100));
    let [step] = steps(&committed)[..] else {
        panic!("expected one step, got {committed:?}");
    };
    assert!(
        matches!(step.step, StepKind::FirmwareZero { .. }),
        "{step:?}"
    );
    assert!(step.proposed.ccd().is_zero());
    // The reset is issued once: asking again owes nothing.
    assert!(journal.settle_day(&same_boot("day1", 60_200)).is_empty());
}

#[test]
fn a_boot_that_held_never_promotes_later_in_the_same_boot() {
    let mut journal = Journal::ready();
    journal.boot_start("day1");
    let held = journal.settle_day(&cold_boot("day1", 50));
    assert!(
        steps(&held)
            .iter()
            .all(|step| matches!(step.step, StepKind::Hold { .. })),
        "{held:?}"
    );
    // A night qualifies the proposal while the boot still runs; asking again in that boot owes no promotion.
    journal.until_filler(|_| Ending::Pass);
    assert!(
        steps(&journal.settle_day(&cold_boot("day1", 60_000))).is_empty(),
        "a boot gets one promotion decision"
    );
    // The next cold boot promotes.
    journal.boot_start("day2");
    let committed = journal.settle_day(&cold_boot("day2", 61_000));
    assert!(
        steps(&committed)
            .iter()
            .any(|step| matches!(step.step, StepKind::Promote { .. })),
        "{committed:?}"
    );
}

/// A corrected or uncorrected machine check in bank `bank_type`, reported by core 1 when `core` says so.
fn machine_check(bank_type: BankType, corrected: bool, core: bool) -> LiveFault {
    LiveFault::MachineCheck {
        cpu: CpuId::from(2_u16),
        core: core.then(|| CoreId::new(1).unwrap()),
        bank: 0,
        bank_type,
        corrected,
    }
}

#[test_case(BankType::L3Cache, true, true, false; "a corrected shared bank with its core is not CPU evidence")]
#[test_case(BankType::MemoryController, true, true, false; "a corrected memory controller with its core is not CPU evidence")]
#[test_case(BankType::DataFabric, true, true, false; "a corrected data fabric with its core is not CPU evidence")]
#[test_case(BankType::Unknown, true, true, false; "a corrected unknown bank with its core is not CPU evidence")]
#[test_case(BankType::L2Cache, true, false, true; "a corrected core-local bank without a resolved core is CPU evidence")]
#[test_case(BankType::ExecutionUnit, true, true, true; "a corrected core-local bank with its core is CPU evidence")]
#[test_case(BankType::L3Cache, false, false, true; "an uncorrected shared bank is CPU evidence")]
#[test_case(BankType::Unknown, false, true, true; "an uncorrected unknown bank is CPU evidence")]
fn a_live_machine_check_is_cpu_evidence_by_its_bank(
    bank_type: BankType,
    corrected: bool,
    core: bool,
    cpu: bool,
) {
    let (mut journal, d) = Journal::promoted_compact();
    journal.append(
        DayFault {
            fault: machine_check(bank_type, corrected, core),
            monotonic_ns: 1_000_000_000,
            offsets: offsets_of(&d),
            lines: Vec::new(),
        }
        .into(),
        &[],
    );
    let committed = journal.settle_day(&same_boot("day1", 60_100));
    let events: Vec<_> = committed
        .iter()
        .filter_map(|payload| {
            if let Payload::DayEvent(event) = payload {
                Some(event)
            } else {
                None
            }
        })
        .collect();
    if cpu {
        assert!(
            matches!(
                events.as_slice(),
                [event] if matches!(event.classification, Classification::CpuSuspect { .. })
            ),
            "{committed:?}"
        );
    } else {
        assert!(events.is_empty(), "{committed:?}");
        assert!(
            !committed
                .iter()
                .any(|payload| matches!(payload, Payload::DayVeto(_))),
            "{committed:?}"
        );
    }
}

/// Runs the first night, failing every qualification trial, until a qualification fails conclusively; its
/// profile is P.
fn failed_qualification(journal: &mut Journal) -> CcdProfile {
    for step in 0..3_500_i64 {
        let plan = journal.until_trial(at(100 + step));
        if let Some(failed) = qualifications(journal).into_iter().find(|record| {
            record.purpose == QualificationPurpose::NextStep
                && matches!(record.state, QualificationState::Failed { .. })
        }) {
            return failed.profile.ccd().clone();
        }
        let ending = if plan.activity == Activity::Qualification {
            Ending::Fail(None)
        } else {
            Ending::Pass
        };
        journal.run(&plan, ending);
    }
    panic!("no qualification failed");
}

#[test]
fn a_proposal_that_failed_qualification_holds_with_its_witnesses_and_no_daily_event() {
    let mut journal = Journal::ready();
    let failed = failed_qualification(&mut journal);
    journal.boot_start("day1");
    let committed = journal.settle_day(&cold_boot("day1", 60_000));
    let [step] = steps(&committed)[..] else {
        panic!("expected one step, got {committed:?}");
    };
    let StepKind::Hold { blocked_by } = &step.step else {
        panic!("expected a hold, got {step:?}");
    };
    let witnesses = blocked_by
        .iter()
        .find_map(|reason| {
            if let HoldReason::QualificationFailed { profile, witnesses } = reason
                && *profile.ccd() == failed
            {
                Some(witnesses)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no failed qualification in {blocked_by:?}"));
    assert!(!witnesses.is_empty());
    assert!(
        committed
            .iter()
            .all(|payload| !matches!(payload, Payload::DayEvent(_) | Payload::DayVeto(_))),
        "{committed:?}"
    );
}
