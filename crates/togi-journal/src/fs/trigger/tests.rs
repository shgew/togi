use super::{AppendPoint, AppendTrigger};
use crate::fs::MemFs;
use crate::testing::{
    begin_default, boot, default_session, live, note, reopen, simulated, stamp, start,
};

/// The complete lines on disk.
fn lines(fs: &MemFs) -> usize {
    live(fs)
        .split_inclusive(|&byte| byte == b'\n')
        .filter(|line| line.ends_with(b"\n"))
        .count()
}

#[test]
fn a_trigger_at_the_first_append_fails_it_before_its_line_is_written() {
    let fs = MemFs::new();
    let mut journal = simulated(&fs).open(boot(1)).unwrap();
    let trigger = AppendTrigger::new(1, AppendPoint::BeforeWrite);
    assert!(trigger.arm(&fs, 0));
    assert!(
        journal
            .append(stamp(0), start(default_session()), &[])
            .is_err()
    );
    assert!(trigger.disarm(&fs));
    assert_eq!(lines(&fs), 0);
    drop(journal);

    assert!(!trigger.arm(&fs, 0), "the trigger fired once");
    let mut journal = reopen(&fs, 2);
    journal
        .append(stamp(1), start(default_session()), &[])
        .unwrap();
    assert!(!trigger.disarm(&fs));
    assert_eq!(lines(&fs), 1);
}

#[test]
fn a_trigger_at_a_later_append_fails_its_sync_after_the_line_is_written() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let trigger = AppendTrigger::new(3, AppendPoint::BeforeSync);
    assert!(trigger.arm(&fs, 1));
    journal.append(stamp(1), note("two"), &[]).unwrap();
    assert!(!trigger.fired());
    assert!(journal.append(stamp(2), note("three"), &[]).is_err());
    assert!(trigger.disarm(&fs));
    assert!(trigger.fired());
    assert_eq!(lines(&fs), 3);
}

#[test]
fn a_trigger_at_a_line_the_journal_already_holds_never_arms() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    journal.append(stamp(1), note("two"), &[]).unwrap();
    let trigger = AppendTrigger::new(2, AppendPoint::BeforeWrite);
    assert!(!trigger.arm(&fs, 2));
    journal.append(stamp(2), note("three"), &[]).unwrap();
    assert!(!trigger.disarm(&fs));
    assert!(!trigger.fired());
    assert!(trigger.reached(2));
}

#[test]
fn a_trigger_spanning_a_resumed_boot_fires_at_its_line_in_the_later_boot() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let trigger = AppendTrigger::new(4, AppendPoint::BeforeWrite);
    assert!(trigger.arm(&fs, 1));
    journal.append(stamp(1), note("two"), &[]).unwrap();
    drop(journal);
    assert!(
        !trigger.disarm(&fs),
        "the first boot stopped short of the line"
    );
    assert!(!trigger.reached(2));

    let mut journal = reopen(&fs, 2);
    assert!(trigger.arm(&fs, journal.events().len()));
    journal.append(stamp(2), note("three"), &[]).unwrap();
    assert!(journal.append(stamp(3), note("four"), &[]).is_err());
    assert!(trigger.disarm(&fs));
    assert_eq!(lines(&fs), 3);
    assert!(trigger.reached(3));
    assert!(!trigger.arm(&fs, 3), "the trigger fired once");
}

#[test]
fn disarming_a_boot_that_stopped_short_withdraws_its_fault() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let trigger = AppendTrigger::new(3, AppendPoint::BeforeWrite);
    assert!(trigger.arm(&fs, 1));
    journal.append(stamp(1), note("two"), &[]).unwrap();
    assert!(!trigger.disarm(&fs));
    journal.append(stamp(2), note("three"), &[]).unwrap();
    journal.append(stamp(3), note("four"), &[]).unwrap();
    assert_eq!(lines(&fs), 4);
}

#[test]
fn a_landed_trigger_reports_its_appended_event_once() {
    let fs = MemFs::new();
    let mut journal = begin_default(&fs);
    let trigger = AppendTrigger::new(3, AppendPoint::Landed);
    let two = journal.append(stamp(1), note("two"), &[]).unwrap().clone();
    assert!(!trigger.lands(&two), "an unarmed trigger never fires");
    assert!(trigger.arm(&fs, 2));
    assert!(!trigger.lands(&two));
    let three = journal
        .append(stamp(2), note("three"), &[])
        .unwrap()
        .clone();
    assert!(trigger.lands(&three));
    assert!(trigger.fired());
    assert!(!trigger.lands(&three), "the trigger fired once");
    assert!(trigger.disarm(&fs));
    assert!(!trigger.arm(&fs, 3));
}
