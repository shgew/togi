//! The unclean-end evidence in the operator's words: each kind of evidence `togi status` lists under "togi saw",
//! pinned as a snapshot.

use test_case::test_case;
use togi_journal::DayUncleanEnd;
use togi_machine::{
    EndEvidence, GraphicsDriver, KernelFault, KernelTail, MceCount, PerCore, ResetKind,
    ResetReason, SleepMode,
};

use crate::day::observed;

fn read(corrected: u32, uncorrected: u32) -> MceCount {
    MceCount::Read {
        corrected,
        uncorrected,
    }
}

fn faults(faults: &[KernelFault], logging_continued: bool) -> KernelTail {
    KernelTail::Faults {
        faults: faults.to_vec(),
        logging_continued,
    }
}

fn rendered(crashed_mces: MceCount, successor_mces: MceCount, tail: KernelTail) -> String {
    let end = DayUncleanEnd {
        boot: "7a1b2c3d-0000-0000-0000-000000000000".to_owned().into(),
        last_checkpoint: None,
        offsets: PerCore::default(),
        reset_reason: ResetReason {
            kind: Some(ResetKind::Watchdog),
            raw: Vec::new(),
            supported: true,
        },
        evidence: EndEvidence {
            crashed_mces,
            successor_mces,
            tail,
            asleep: None,
        },
    };
    observed(&end).join("\n") + "\n"
}

#[test_case(
    read(3, 1), read(0, 2), KernelTail::Quiet {},
    include_str!("day/machine-checks-both-boots.txt");
    "machine checks list corrected before uncorrected, boot by boot"
)]
#[test_case(
    MceCount::Unknown {}, MceCount::Unknown {}, KernelTail::Unreadable {},
    include_str!("day/unreadable-tail.txt");
    "an unreadable tail and unread machine checks are unknown"
)]
#[test_case(
    read(0, 0), read(0, 0), KernelTail::Missing {},
    include_str!("day/missing-tail.txt");
    "a missing tail is unknown"
)]
#[test_case(
    read(0, 0), MceCount::Unknown {},
    faults(
        &[
            KernelFault::MachineCheck {},
            KernelFault::GraphicsTimeout { driver: GraphicsDriver::Amdgpu },
            KernelFault::GraphicsReset { driver: GraphicsDriver::I915 },
            KernelFault::GraphicsTimeout { driver: GraphicsDriver::Xe },
            KernelFault::NvidiaXid { code: 79 },
            KernelFault::Oops {},
            KernelFault::HungTask {},
        ],
        true,
    ),
    include_str!("day/every-fault-logging-went-on.txt");
    "every fault kind, with logging continued"
)]
#[test_case(
    read(0, 0), MceCount::Unknown {},
    faults(
        &[
            KernelFault::GraphicsReset { driver: GraphicsDriver::Xe },
            KernelFault::GraphicsTimeout { driver: GraphicsDriver::I915 },
        ],
        false,
    ),
    include_str!("day/log-ended-at-the-last-fault.txt");
    "the log ending at the last fault"
)]
fn the_evidence_is_worded_for_the_operator(
    crashed: MceCount,
    successor: MceCount,
    tail: KernelTail,
    expected: &str,
) {
    assert_eq!(rendered(crashed, successor, tail), expected);
}

#[test]
fn a_log_that_ended_at_a_sleep_entry_says_so_and_reads_as_a_sleep_watchdog_reset() {
    let mut end = DayUncleanEnd {
        boot: "7a1b2c3d-0000-0000-0000-000000000000".to_owned().into(),
        last_checkpoint: None,
        offsets: PerCore::default(),
        reset_reason: ResetReason {
            kind: Some(ResetKind::Watchdog),
            raw: Vec::new(),
            supported: true,
        },
        evidence: EndEvidence::unknown(),
    };
    end.evidence.asleep = Some(SleepMode::Suspend);
    assert_eq!(
        observed(&end).last().map(String::as_str),
        Some("the log ended at a suspend entry")
    );
    assert_eq!(
        crate::day::class_words(end.class()),
        "a watchdog reset while the machine was going to sleep"
    );
}
