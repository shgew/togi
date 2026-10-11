use super::fixture::{
    Fake, Gate, Tree, lock, read, smu_error, validated, write, write_all, zero_all,
};
use crate::mailbox::{Command, MailboxError};
use crate::{Driver, Error};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;
use togi_machine::TargetOffset;

const SHORT: Duration = Duration::from_millis(20);

fn blocking_driver() -> (Tree, Driver, Fake, Gate) {
    let tree = Tree::new(8, true);
    tree.identity_files();
    let fake = Fake::new();
    let gate = Gate::default();
    let held = gate.clone();
    fake.hook_command(move || held.wait());
    let mut driver = Driver::with_transport(tree.root(), Box::new(fake.clone()), SHORT).unwrap();
    let _ = driver.validate().unwrap();
    (tree, driver, fake, gate)
}

#[test]
fn an_overrun_poisons_the_driver_and_starts_no_second_transaction() {
    let (_tree, mut driver, fake, gate) = blocking_driver();
    let error = read(&mut driver, 0).unwrap_err();
    assert!(matches!(
        smu_error(&error),
        Error::Mailbox {
            source: MailboxError::TimedOut { deadline, .. },
            ..
        } if *deadline == SHORT
    ));
    assert_eq!(fake.commands().len(), 1);

    let target = TargetOffset::clamped(-10);
    let proof = driver.validate().unwrap();
    for refused in [
        read(&mut driver, 1).map(drop),
        write(&mut driver, 1, target),
        write_all(&mut driver, target),
        zero_all(&mut driver, proof),
        driver.bios_context().map(drop).map_err(Into::into),
    ] {
        assert!(matches!(
            smu_error(&refused.unwrap_err()),
            Error::Mailbox {
                source: MailboxError::Poisoned,
                ..
            }
        ));
    }
    gate.open();
    assert!(matches!(
        smu_error(&read(&mut driver, 2).unwrap_err()),
        Error::Mailbox {
            source: MailboxError::Poisoned,
            ..
        }
    ));
    assert_eq!(
        fake.commands().len(),
        1,
        "a transaction started on a poisoned driver"
    );
}

#[test]
fn the_timeout_error_names_the_stuck_command() {
    let (_tree, mut driver, _fake, gate) = blocking_driver();
    let error = read(&mut driver, 0).unwrap_err();
    let text = crate::error::chain(smu_error(&error));
    assert!(
        text.contains("RSMU command 0xd5 did not finish within 20ms"),
        "{text}"
    );
    gate.open();
}

#[test]
fn a_blocked_smn_read_during_validation_leaves_slot_mapping_refused() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let gate = Gate::default();
    let held = gate.clone();
    fake.hook_smn(move |_, value| {
        held.wait();
        value
    });
    let mut driver = Driver::with_transport(tree.root(), Box::new(fake.clone()), SHORT).unwrap();
    let _ = driver.validate().unwrap();
    let check = driver.check_slot_mapping();
    assert!(!check.ok);
    assert!(
        check.detail.contains("did not finish within"),
        "{}",
        check.detail
    );
    assert_eq!(fake.smn_reads().len(), 1);
    gate.open();
}

#[test]
fn every_transaction_runs_on_the_one_worker_thread() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let threads: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
    let recorded = Arc::clone(&threads);
    fake.hook_command(move || lock(&recorded).push(thread::current().id()));
    let recorded = Arc::clone(&threads);
    fake.hook_smn(move |_, value| {
        lock(&recorded).push(thread::current().id());
        value
    });
    let mut driver = validated(&tree, &fake);
    read(&mut driver, 0).unwrap();
    write(&mut driver, 0, TargetOffset::ZERO).unwrap();
    let threads = lock(&threads);
    assert_eq!(threads.len(), 10);
    assert!(threads.iter().all(|id| *id == threads[0]));
    assert_ne!(threads[0], thread::current().id());
}

#[test]
fn a_panicking_transport_loses_the_worker_and_poisons() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    fake.hook_command(|| panic!("transport failed"));
    let mut driver = validated(&tree, &fake);
    let first = read(&mut driver, 0).unwrap_err();
    assert!(matches!(
        smu_error(&first),
        Error::Mailbox {
            source: MailboxError::WorkerLost,
            ..
        }
    ));
    let second = read(&mut driver, 0).unwrap_err();
    assert!(matches!(
        smu_error(&second),
        Error::Mailbox {
            source: MailboxError::Poisoned,
            ..
        }
    ));
}

#[test]
fn a_command_is_one_transaction_with_its_encoded_argument() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    read(&mut driver, 3).unwrap();
    assert_eq!(fake.commands(), [(Command::ReadOffset, 0x0030_0000)]);
}
