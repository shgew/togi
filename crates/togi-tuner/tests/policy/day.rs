//! The day query through the journal: what a boot owes once it has a step.

use togi_journal::StepKind;
use togi_machine::BootId;
use togi_tuner::day::Invocation;
use togi_tuner::policy::DayQuery;

use crate::fixture::{Ending, Journal, at, firmware};
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
