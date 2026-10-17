use super::fixture::{
    Fake, GENEROUS, Gate, Tree, lock, open_within, read, smu_error, validated, write, write_all,
    zero_all,
};
use crate::mailbox::{Command, MailboxError, Request, Worker};
use crate::pm_table::{PmFile, TABLE_LEN};
use crate::{Driver, Error, Operation, PmReader};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;
use togi_machine::{PmTableReader, TargetOffset};

/// Short enough to keep the timeout tests quick, long enough that a worker that is already
/// running claims its job well inside it.
const OVERRUN: Duration = Duration::from_millis(200);

/// Only used where the outcome cannot depend on scheduling: the job can never start in time.
const SHORT: Duration = Duration::from_millis(20);

/// A validated driver whose next mailbox command blocks until `release`, with `entered` opening
/// once that command reached the transport.
struct Blocked {
    _tree: Tree,
    driver: Driver,
    fake: Fake,
    entered: Gate,
    release: Gate,
}

impl Blocked {
    fn new() -> Self {
        let tree = Tree::new(8, true);
        tree.identity_files();
        let fake = Fake::new();
        let entered = Gate::default();
        let release = Gate::default();
        let (opened, held) = (entered.clone(), release.clone());
        fake.hook_command(move || {
            opened.open();
            held.wait();
        });
        // Validation reads SMN registers and must not race the short deadline; only the
        // transactions after it do.
        let mut driver = open_within(&tree, &fake, GENEROUS);
        let _ = driver.validate().unwrap();
        driver.set_deadline(OVERRUN);
        Self {
            _tree: tree,
            driver,
            fake,
            entered,
            release,
        }
    }

    /// Proves the blocked transaction reached the transport before anything is asserted about it.
    fn assert_entered(&self) {
        assert!(
            self.entered.wait_timeout(GENEROUS),
            "the blocked transaction never reached the transport"
        );
    }
}

fn poisoned_by(error: &togi_machine::Error, stuck: Request) {
    assert!(
        matches!(
            smu_error(error),
            Error::Mailbox { source: MailboxError::Poisoned { request }, .. } if *request == stuck
        ),
        "{error:?}"
    );
}

#[test]
fn an_overrun_poisons_the_driver_and_starts_no_second_transaction() {
    let mut blocked = Blocked::new();
    let error = read(&mut blocked.driver, 0).unwrap_err();
    assert!(matches!(
        smu_error(&error),
        Error::Mailbox {
            source: MailboxError::TimedOut { deadline, .. },
            ..
        } if *deadline == OVERRUN
    ));
    blocked.assert_entered();
    assert_eq!(blocked.fake.commands().len(), 1);

    let stuck = Request::Command(Command::ReadOffset);
    let target = TargetOffset::clamped(-10);
    let proof = blocked.driver.validate().unwrap();
    for refused in [
        read(&mut blocked.driver, 1).map(drop),
        write(&mut blocked.driver, 1, target),
        write_all(&mut blocked.driver, target),
        zero_all(&mut blocked.driver, proof),
        blocked.driver.bios_context().map(drop).map_err(Into::into),
    ] {
        poisoned_by(&refused.unwrap_err(), stuck);
    }
    blocked.release.open();
    poisoned_by(&read(&mut blocked.driver, 2).unwrap_err(), stuck);
    assert_eq!(
        blocked.fake.commands().len(),
        1,
        "a transaction started on a poisoned driver"
    );
}

#[test]
fn a_refusal_after_a_timeout_names_the_stuck_request() {
    let mut blocked = Blocked::new();
    let _ = read(&mut blocked.driver, 0).unwrap_err();
    blocked.assert_entered();
    let proof = blocked.driver.validate().unwrap();
    let refused = zero_all(&mut blocked.driver, proof).unwrap_err();
    let text = togi_machine::error_chain(smu_error(&refused));
    assert!(
        text.starts_with("zero all offsets: RSMU command 0xd5 did not finish in time earlier"),
        "{text}"
    );
    blocked.release.open();
}

#[test]
fn the_timeout_error_names_the_stuck_command() {
    let mut blocked = Blocked::new();
    let error = read(&mut blocked.driver, 0).unwrap_err();
    let text = togi_machine::error_chain(smu_error(&error));
    assert!(
        text.contains(&format!(
            "RSMU command 0xd5 did not finish within {OVERRUN:?}"
        )),
        "{text}"
    );
    blocked.release.open();
}

#[test]
fn a_blocked_smn_read_during_validation_leaves_slot_mapping_refused() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let entered = Gate::default();
    let release = Gate::default();
    let (opened, held) = (entered.clone(), release.clone());
    fake.hook_smn(move |_, value| {
        opened.open();
        held.wait();
        value
    });
    let mut driver = open_within(&tree, &fake, OVERRUN);
    let _ = driver.validate().unwrap();
    assert!(
        entered.wait_timeout(GENEROUS),
        "the blocked read never reached the transport"
    );
    let check = driver.check_slot_mapping();
    assert!(!check.ok);
    assert!(
        check.detail.contains("did not finish within"),
        "{}",
        check.detail
    );
    assert_eq!(fake.smn_reads().len(), 1);
    release.open();
}

#[test]
fn a_job_that_expired_while_queued_never_reaches_the_transport() {
    let fake = Fake::new();
    let entered = Gate::default();
    let release = Gate::default();
    let (opened, held) = (entered.clone(), release.clone());
    fake.hook_command(move || {
        opened.open();
        held.wait();
    });
    let worker = Worker::spawn(Box::new(fake.clone()), SHORT).unwrap();
    let first = worker
        .submit(Request::Command(Command::ReadOffset), |transport| {
            transport.command(Command::ReadOffset, [0; 6])
        })
        .unwrap();
    assert!(
        entered.wait_timeout(GENEROUS),
        "the first job never reached the transport"
    );
    // The worker is stuck in the first job, so this one cannot start before its caller gives up.
    let second = worker
        .submit(Request::Command(Command::SetAllOffsets), |transport| {
            transport.command(Command::SetAllOffsets, [0; 6])
        })
        .unwrap();
    let error = worker.finish(second).unwrap_err();
    assert!(matches!(
        error,
        MailboxError::TimedOut {
            request: Request::Command(Command::SetAllOffsets),
            ..
        }
    ));

    release.open();
    worker.flush();
    assert_eq!(
        fake.commands(),
        [(Command::ReadOffset, 0)],
        "an expired job started after its caller was told it failed"
    );
    drop(first);
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
    poisoned_by(&second, Request::Command(Command::ReadOffset));
}

#[test]
fn a_command_is_one_transaction_with_its_encoded_argument() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    read(&mut driver, 3).unwrap();
    assert_eq!(fake.commands(), [(Command::ReadOffset, 0x0030_0000)]);
}

fn serve_supported_pm(fake: &Fake) {
    fake.serve_pm(Some(&[0x05, 0x02, 0x62, 0x00]), Some(&[0; TABLE_LEN]));
}

fn settle(reader: &PmReader) {
    assert!(reader.wait_idle(GENEROUS), "the PM read never finished");
}

#[test]
fn no_pm_reader_exists_once_a_mailbox_transaction_overran() {
    let mut blocked = Blocked::new();
    serve_supported_pm(&blocked.fake);
    let _ = read(&mut blocked.driver, 0).unwrap_err();
    blocked.assert_entered();

    let refused = blocked.driver.pm_reader().unwrap_err();
    assert!(
        matches!(
            refused,
            Error::Mailbox {
                operation: Operation::ReadPmTable,
                source: MailboxError::Poisoned {
                    request: Request::Command(Command::ReadOffset)
                },
            }
        ),
        "{refused:?}"
    );
    assert_eq!(blocked.fake.pm_reads(PmFile::Version), 0);
    assert_eq!(blocked.fake.pm_reads(PmFile::Table), 0);
    blocked.release.open();
}

#[test]
fn an_existing_pm_reader_starts_no_transfer_once_a_mailbox_transaction_overran() {
    let mut blocked = Blocked::new();
    serve_supported_pm(&blocked.fake);
    let reader = blocked.driver.pm_reader().unwrap();
    settle(&reader);
    assert_eq!(
        blocked.fake.pm_reads(PmFile::Version),
        1,
        "positive control"
    );
    assert_eq!(blocked.fake.pm_reads(PmFile::Table), 1, "positive control");

    let _ = read(&mut blocked.driver, 0).unwrap_err();
    blocked.assert_entered();

    assert!(reader.check().ok);
    settle(&reader);
    let _ = reader.pm_table();
    settle(&reader);
    assert_eq!(
        blocked.fake.pm_reads(PmFile::Version),
        1,
        "a PM version read started on a poisoned driver"
    );
    assert_eq!(
        blocked.fake.pm_reads(PmFile::Table),
        1,
        "a PM table read started on a poisoned driver"
    );
    assert!(matches!(
        blocked.driver.pm_reader().unwrap_err(),
        Error::Mailbox {
            source: MailboxError::Poisoned { .. },
            ..
        }
    ));
    blocked.release.open();
}
