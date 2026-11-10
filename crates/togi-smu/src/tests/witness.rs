use super::fixture::{Fake, Tree, core, validated};
use crate::mailbox::Command;
use crate::{OffsetWitness, WitnessError};
use togi_machine::{ObservedOffset, PerCore, TargetOffset};

#[path = "witness_guard.rs"]
mod witness_guard;
use witness_guard::Restoring;

fn fixture() -> OffsetWitness {
    OffsetWitness::fixture((0..16).map(core)).unwrap()
}

#[test]
fn a_witness_requires_all_sixteen_cores() {
    assert!(matches!(
        OffsetWitness::fixture((0..15).map(core)),
        Err(WitnessError::CoreCount { count: 15 })
    ));
}

#[test]
fn a_witness_requires_sixteen_distinct_values() {
    let mut targets = *fixture().targets();
    targets.set(core(15), TargetOffset::new(-1).unwrap());
    assert!(
        matches!(OffsetWitness::new(targets), Err(WitnessError::Duplicate { core: id, .. }) if id == core(15))
    );
}

#[test]
fn a_witness_requires_nonzero_values() {
    let mut targets = *fixture().targets();
    targets.set(core(15), TargetOffset::ZERO);
    assert!(
        matches!(OffsetWitness::new(targets), Err(WitnessError::Zero { core: id }) if id == core(15))
    );
}

#[test]
fn missing_readback_is_not_a_witness() {
    let witness = fixture();
    let observed: PerCore<_> = witness
        .targets()
        .iter()
        .take(15)
        .map(|(id, target)| (id, target.observed()))
        .collect();
    assert!(
        matches!(witness.verify(&observed), Err(WitnessError::Readback { core: id, observed: None, .. }) if id == core(15))
    );
}

#[test]
fn sixteen_distinct_values_reach_their_slots_before_readback_then_stock_is_verified() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    fake.remember_offsets();
    let mut guard = Restoring::new(validated(&tree, &fake));
    let witness = fixture();
    let readback = guard.round_trip(&witness).unwrap();
    for (id, target) in witness.targets().iter() {
        assert_eq!(readback.get(id), Some(target.observed()), "core {id}");
        let slot = u32::from(id.get() / 8) << 28 | u32::from(id.get() % 8) << 20;
        assert_eq!(
            fake.offsets().get(&slot).copied(),
            Some(0x1_0000 - u32::from(id.get()) - 1),
            "core {id}"
        );
    }
    let commands = fake.commands();
    assert_eq!(commands.len(), 32);
    assert!(
        commands[..16]
            .iter()
            .all(|(command, _)| *command == Command::SetOffset)
    );
    assert!(
        commands[16..]
            .iter()
            .all(|(command, _)| *command == Command::ReadOffset)
    );
    assert_eq!(guard.finish(), Vec::<String>::new());
    assert_eq!(fake.offsets().len(), 16);
    assert!(fake.offsets().values().all(|offset| *offset == 0));
    let commands = fake.commands();
    assert_eq!(commands.len(), 64);
    assert!(
        commands[48..]
            .iter()
            .all(|(command, _)| *command == Command::ReadOffset)
    );
}

#[test]
fn mismatched_readback_fails_and_drop_restores_stock() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    fake.remember_offsets();
    fake.respond(Command::ReadOffset, 0);
    {
        let mut guard = Restoring::new(validated(&tree, &fake));
        let error = guard.round_trip(&fixture()).unwrap_err();
        assert!(
            matches!(error, witness_guard::RoundTripError::Readback(WitnessError::Readback { core: id, observed: Some(ObservedOffset(0)), .. }) if id == core(0))
        );
    }
    assert_eq!(fake.offsets().len(), 16);
    assert!(fake.offsets().values().all(|offset| *offset == 0));
}

#[test]
fn every_failed_witness_access_still_restores_and_reads_back_all_stock_slots() {
    // Exhaust every write/read boundary of the sixteen-core round trip.
    for failed in 1..=32 {
        let tree = Tree::new(8, true);
        let fake = Fake::new();
        fake.remember_offsets();
        fake.fail_command_at(failed);
        {
            let mut guard = Restoring::new(validated(&tree, &fake));
            assert!(
                matches!(
                    guard.round_trip(&fixture()),
                    Err(witness_guard::RoundTripError::Access(_))
                ),
                "access {failed}"
            );
        }
        assert_eq!(fake.offsets().len(), 16, "access {failed}");
        assert!(
            fake.offsets().values().all(|offset| *offset == 0),
            "access {failed}"
        );
        let commands = fake.commands();
        assert_eq!(commands.len(), failed + 32, "access {failed}");
        assert!(
            commands[failed..failed + 16]
                .iter()
                .all(|(command, arg)| *command == Command::SetOffset && arg.trailing_zeros() >= 16),
            "access {failed}"
        );
        assert!(
            commands[failed + 16..]
                .iter()
                .all(|(command, _)| *command == Command::ReadOffset),
            "access {failed}"
        );
    }
}

#[test]
fn panic_after_nonzero_writes_restores_stock() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    fake.remember_offsets();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut guard = Restoring::new(validated(&tree, &fake));
        guard.round_trip(&fixture()).unwrap();
        panic!("fail after writing the witness");
    }));
    assert!(result.is_err());
    assert_eq!(fake.offsets().len(), 16);
    assert!(fake.offsets().values().all(|offset| *offset == 0));
}

#[test]
fn every_failed_stock_access_is_reported_without_skipping_other_cores() {
    // Exhaust every cleanup write/read boundary; Drop makes another bounded stock attempt.
    for failed in 33..=64 {
        let tree = Tree::new(8, true);
        let fake = Fake::new();
        fake.remember_offsets();
        fake.fail_command_at(failed);
        let mut guard = Restoring::new(validated(&tree, &fake));
        guard.round_trip(&fixture()).unwrap();
        assert!(!guard.finish().is_empty(), "access {failed}");
        assert_eq!(fake.commands().len(), 96, "access {failed}");
        assert!(
            fake.offsets().values().all(|offset| *offset == 0),
            "access {failed}"
        );
    }
}

#[test]
fn nonzero_stock_readback_never_reports_success() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    fake.remember_offsets();
    let mut guard = Restoring::new(validated(&tree, &fake));
    guard.round_trip(&fixture()).unwrap();
    fake.respond(Command::ReadOffset, 0xffff);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| guard.finish()));
    assert!(result.is_err());
    assert_eq!(fake.commands().len(), 96);
    assert!(fake.offsets().values().all(|offset| *offset == 0));
}
