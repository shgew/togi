//! The day query through the journal: what a boot owes once it has a step, and which live machine checks
//! are CPU evidence.

use test_case::test_case;
use togi_journal::{Classification, DayFault, LiveFault, Payload, StepKind};
use togi_machine::{BankType, BootId, CoreId, CpuId};
use togi_tuner::day::Invocation;
use togi_tuner::policy::DayQuery;

use crate::fixture::{Ending, Journal, at, firmware, offsets_of};
use crate::flow::{cold_boot, promoted, steps};

fn same_boot(boot: &str, second: i64) -> DayQuery {
    DayQuery {
        boot: BootId::from(boot.to_owned()),
        invocation: Invocation::SameBoot,
        now: at(second),
    }
}

#[test]
fn a_firmware_change_later_in_the_boot_still_zeroes_d_on_a_same_boot_query() {
    let (mut journal, _) = promoted();
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
    assert!(step.proposed.ccd.is_zero());
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
    let (mut journal, d) = promoted();
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
