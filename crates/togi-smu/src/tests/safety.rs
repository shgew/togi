use super::fixture::{
    Fake, Tree, accepted_unvalidated, forged_proof, open, read, smu_error, validated, write,
    write_all, zero_all,
};
use crate::mailbox::Command;
use crate::{Driver, Error, Operation};
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
    let _ = driver.validate().unwrap();
    let reader = driver.pm_reader().unwrap();
    let check = reader.check();
    assert!(check.ok);
    assert!(
        check.detail.contains("per-core lanes absent"),
        "{}",
        check.detail
    );
    assert!(reader.pm_table().is_none());
}

#[test]
fn targets_are_clamped_before_they_reach_the_mailbox() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    for (target, word) in [(-60, 0xffce), (-51, 0xffce), (-50, 0xffce), (5, 0), (1, 0)] {
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
}

#[test]
fn readbacks_outside_the_writable_range_are_reported_raw() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    for (raw, want) in [
        (0x000a, 10),
        (0xffc4, -60),
        (0x7fff, 32767),
        (0x8000, -32768),
    ] {
        fake.respond(Command::ReadOffset, raw);
        assert_eq!(read(&mut driver, 4).unwrap(), ObservedOffset(want));
    }
    let proof = driver.validate().unwrap();
    zero_all(&mut driver, proof).unwrap();
    for (raw, want) in [(0x000a, 10), (0xffc4, -60)] {
        fake.respond(Command::ReadOffset, raw);
        assert_eq!(read(&mut driver, 4).unwrap(), ObservedOffset(want));
    }
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
    let mut identity = Driver::identity_only(tree.root()).unwrap();
    let error = zero_all(&mut identity, forged_proof()).unwrap_err();
    assert!(matches!(smu_error(&error), Error::IdentityOnly { .. }));
    assert_eq!(fake.hardware_calls(), 0);
}
