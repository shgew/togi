#[cfg(target_os = "linux")]
use super::fixture::Watch;
use super::fixture::{
    Fake, GENEROUS, Tree, accepted_unvalidated, forged_proof, open, read, smu_error, validated,
    write, write_all, zero_all,
};
use crate::mailbox::Command;
use crate::pm_table::PmFile;
use crate::{Driver, Error, Operation};
use test_case::test_case;
use togi_machine::{ObservedOffset, PmTableReader, TargetOffset};

#[test]
fn nothing_reaches_the_mailbox_before_validation() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = open(&tree, &fake);
    assert_eq!(accepted_unvalidated(&mut driver), Vec::<&str>::new());
    assert_eq!(fake.hardware_calls(), 0);
}

#[test]
fn the_pm_table_reader_exists_only_for_a_validated_driver() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = open(&tree, &fake);
    assert!(matches!(
        driver.pm_reader().unwrap_err(),
        Error::NotValidated {
            operation: Operation::ReadPmTable
        }
    ));
    assert_eq!(
        fake.hardware_calls(),
        0,
        "a refused reader touched hardware"
    );
    let _ = driver.validate().unwrap();
    let calls = fake.hardware_calls();
    assert_eq!(fake.pm_reads(PmFile::Version), 0, "validate read a PM file");
    let reader = driver.pm_reader().unwrap();
    assert!(reader.wait_idle(GENEROUS), "the first read never finished");
    assert_eq!(
        fake.pm_reads(PmFile::Version),
        1,
        "positive control: a validated reader reads the version"
    );
    assert_eq!(fake.hardware_calls(), calls + 1);
    let check = reader.check();
    assert!(check.ok);
    assert!(
        check.detail.contains("per-core lanes absent"),
        "{}",
        check.detail
    );
    assert!(reader.pm_table().is_none());
}

#[test_case(-60, 0xffce; "below the floor")]
#[test_case(-51, 0xffce; "one below the floor")]
#[test_case(-50, 0xffce; "at the floor")]
#[test_case(5, 0; "above zero")]
#[test_case(1, 0; "one above zero")]
fn targets_are_clamped_before_they_reach_the_mailbox(target: i64, word: u32) {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let target = TargetOffset::clamped(target);
    write(&mut driver, 9, target).unwrap();
    assert_eq!(
        fake.commands().last(),
        Some(&(Command::SetOffset, 0x1010_0000 | word))
    );
    write_all(&mut driver, target).unwrap();
    assert_eq!(
        fake.commands().last(),
        Some(&(Command::SetAllOffsets, word))
    );
}

#[test_case(0x000a, 10; "positive")]
#[test_case(0xffc4, -60; "below the writable range")]
#[test_case(0x7fff, 32767; "i16 max")]
#[test_case(0x8000, -32768; "i16 min")]
fn readbacks_outside_the_writable_range_are_reported_raw(raw: u32, want: i64) {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    fake.respond(Command::ReadOffset, raw);
    assert_eq!(read(&mut driver, 4).unwrap(), ObservedOffset(want));
}

#[test_case(0x000a, 10; "positive")]
#[test_case(0xffc4, -60; "below the writable range")]
fn readbacks_are_reported_raw_after_an_emergency_zero(raw: u32, want: i64) {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let proof = driver.validate().unwrap();
    zero_all(&mut driver, proof).unwrap();
    fake.respond(Command::ReadOffset, raw);
    assert_eq!(read(&mut driver, 4).unwrap(), ObservedOffset(want));
}

#[test]
fn emergency_zero_writes_exactly_one_all_core_zero() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let proof = driver.validate().unwrap();
    zero_all(&mut driver, proof).unwrap();
    assert_eq!(fake.commands(), [(Command::SetAllOffsets, 0)]);
}

#[test]
fn emergency_zero_still_writes_when_slot_mapping_was_refused() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    fake.set_value(0x304a_03dc, 0x01);
    let mut driver = validated(&tree, &fake);
    assert!(!driver.check_slot_mapping().ok);
    assert!(read(&mut driver, 0).is_err());
    let proof = driver.validate().unwrap();
    zero_all(&mut driver, proof).unwrap();
    assert_eq!(fake.commands(), [(Command::SetAllOffsets, 0)]);
}

#[test]
fn emergency_zero_needs_a_validated_driver_even_with_a_proof() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut unvalidated = open(&tree, &fake);
    let error = zero_all(&mut unvalidated, forged_proof()).unwrap_err();
    assert!(matches!(
        smu_error(&error),
        Error::NotValidated {
            operation: Operation::EmergencyZero
        }
    ));
    assert_eq!(fake.hardware_calls(), 0);
    let mut identity = tree.identity_only();
    let error = zero_all(&mut identity, forged_proof()).unwrap_err();
    assert!(matches!(smu_error(&error), Error::IdentityOnly { .. }));
}

/// Every way into an identity-only driver: the checks that read identity files, then every entry
/// point that would reach the hardware, including emergency zeroing with a forged proof.
fn exercise_identity_only(driver: &mut Driver) {
    assert!(driver.check_cpu().ok);
    assert!(driver.check_driver().ok);
    assert!(matches!(
        driver.validate().unwrap_err(),
        Error::IdentityOnly {
            operation: Operation::Validate
        }
    ));
    assert_eq!(accepted_unvalidated(driver), Vec::<&str>::new());
    let error = zero_all(driver, forged_proof()).unwrap_err();
    assert!(matches!(smu_error(&error), Error::IdentityOnly { .. }));
    assert!(driver.topology().is_ok());
}

#[test]
fn an_identity_only_driver_refuses_every_entry_point() {
    let tree = Tree::new(8, true);
    tree.identity_files();
    let mut driver = tree.identity_only();
    exercise_identity_only(&mut driver);
}

/// The production `identity_only` constructor, with an inotify watch on every mailbox and PM
/// table file: not one of them may be opened, read or written.
#[cfg(target_os = "linux")]
#[test]
fn real_fs_an_identity_only_driver_opens_no_mailbox_or_pm_table_file() {
    let tree = Tree::real(8, true);
    tree.identity_files();
    tree.hardware_files();
    let watch = Watch::hardware_files(&tree);
    let mut driver = Driver::identity_only(tree.root()).unwrap();
    assert_eq!(watch.touched(), Vec::<&str>::new(), "construction");
    exercise_identity_only(&mut driver);
    assert_eq!(watch.touched(), Vec::<&str>::new());
}

#[cfg(target_os = "linux")]
#[test]
fn real_fs_an_unvalidated_driver_opens_no_mailbox_or_pm_table_file() {
    let tree = Tree::real(8, true);
    tree.hardware_files();
    let watch = Watch::hardware_files(&tree);
    let mut driver = Driver::open(tree.root()).unwrap();
    assert_eq!(accepted_unvalidated(&mut driver), Vec::<&str>::new());
    assert_eq!(watch.touched(), Vec::<&str>::new());
}

#[cfg(target_os = "linux")]
#[test]
fn real_fs_a_driver_that_failed_its_identity_check_opens_no_mailbox_or_pm_table_file() {
    let tree = Tree::real(8, true);
    tree.put(
        "proc/cpuinfo",
        "cpu family : 25\nmodel : 68\nmodel name : Zen 3\n",
    );
    tree.hardware_files();
    let watch = Watch::hardware_files(&tree);
    let mut driver = Driver::open(tree.root()).unwrap();
    assert!(matches!(
        driver.validate().unwrap_err(),
        Error::UnsupportedCpu { .. }
    ));
    assert_eq!(accepted_unvalidated(&mut driver), Vec::<&str>::new());
    assert_eq!(watch.touched(), Vec::<&str>::new());
}

/// Positive control for the observer above: the same constructor and files, a validated driver
/// that does reach them, and every one shows up.
#[cfg(target_os = "linux")]
#[test]
fn real_fs_the_file_observer_sees_a_validated_driver_reach_the_hardware_files() {
    let tree = Tree::real(8, true);
    tree.hardware_files();
    let watch = Watch::hardware_files(&tree);
    let mut driver = Driver::open(tree.root()).unwrap();
    assert_eq!(watch.touched(), Vec::<&str>::new(), "opening");
    let proof = driver.validate().unwrap();
    assert_eq!(watch.touched(), ["smn"], "validation reads SMN registers");
    let _ = zero_all(&mut driver, proof);
    let reader = driver.pm_reader().unwrap();
    assert!(reader.wait_idle(GENEROUS), "the PM read never finished");
    assert_eq!(
        watch.touched(),
        ["pm_table", "pm_table_version", "rsmu_cmd", "smu_args"]
    );
}

#[test]
fn a_driver_that_failed_its_identity_check_reaches_no_hardware_or_pm_file() {
    let tree = Tree::new(8, true);
    tree.put(
        "proc/cpuinfo",
        "cpu family : 25\nmodel : 68\nmodel name : Zen 3\n",
    );
    let fake = Fake::new();
    let mut driver = open(&tree, &fake);
    assert!(matches!(
        driver.validate().unwrap_err(),
        Error::UnsupportedCpu { .. }
    ));
    assert_eq!(accepted_unvalidated(&mut driver), Vec::<&str>::new());
    assert!(matches!(
        driver.pm_reader().unwrap_err(),
        Error::NotValidated {
            operation: Operation::ReadPmTable
        }
    ));
    assert_eq!(fake.hardware_calls(), 0);
    assert_eq!(fake.pm_reads(PmFile::Version), 0);
    assert_eq!(fake.pm_reads(PmFile::Table), 0);
}

#[test]
fn every_pm_reader_request_shares_the_one_reader() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let first = driver.pm_reader().unwrap();
    let second = driver.pm_reader().unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &second));
    assert_eq!(std::sync::Arc::strong_count(&first), 3);
}
