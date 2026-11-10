use super::fixture::{
    Fake, GENEROUS, Gate, Tree, lock, open_within, read, smu_error, validated, write, write_all,
    zero_all,
};
use crate::mailbox::{Command, MailboxError, Request, Worker};
use crate::pm_table::{PmFile, TABLE_LEN};
#[cfg(target_os = "linux")]
use crate::sysfs::DRIVER_DIR;
use crate::sysfs::read_gated;
use crate::{Driver, Error, Operation, PmReader};
#[cfg(target_os = "linux")]
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, ThreadId};
use std::time::Duration;
use test_case::test_case;
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
        .submit(
            Request::Command(Command::ReadOffset),
            |transport, poison| transport.command(Command::ReadOffset, [0; 6], poison),
        )
        .unwrap();
    assert!(
        entered.wait_timeout(GENEROUS),
        "the first job never reached the transport"
    );
    // The worker is stuck in the first job, so this one cannot start before its caller gives up.
    let second = worker
        .submit(
            Request::Command(Command::SetAllOffsets),
            |transport, poison| transport.command(Command::SetAllOffsets, [0; 6], poison),
        )
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
fn a_hung_transport_poisons_both_command_and_smn_paths_permanently() {
    let fake = Fake::new();
    let entered = Gate::default();
    let release = Gate::default();
    let (opened, held) = (entered.clone(), release.clone());
    fake.hook_command(move || {
        opened.open();
        held.wait();
    });
    let mut worker = Worker::spawn(Box::new(fake.clone()), SHORT).unwrap();
    let ticket = worker
        .submit(
            Request::Command(Command::ReadOffset),
            |transport, poison| transport.command(Command::ReadOffset, [0; 6], poison),
        )
        .unwrap();
    assert!(entered.wait_timeout(GENEROUS));
    let stuck = Request::Command(Command::ReadOffset);
    assert!(matches!(
        worker.finish(ticket).unwrap_err(),
        MailboxError::TimedOut { request, deadline } if request == stuck && deadline == SHORT
    ));
    assert!(matches!(
        worker.command(Command::SetAllOffsets, [0; 6]).unwrap_err(),
        MailboxError::Poisoned { request } if request == stuck
    ));
    assert!(matches!(
        worker.read_smn(0).unwrap_err(),
        MailboxError::Poisoned { request } if request == stuck
    ));

    release.open();
    worker.flush();
    assert!(matches!(
        worker.read_smn(0).unwrap_err(),
        MailboxError::Poisoned { request } if request == stuck
    ));
    assert_eq!(fake.commands(), [(Command::ReadOffset, 0)]);
    assert_eq!(fake.smn_reads(), Vec::<u32>::new());
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

#[test_case("smu_args", false, &["write smu_args"]; "argument write")]
#[test_case("rsmu_cmd", false, &["write smu_args", "write rsmu_cmd"]; "command write")]
#[test_case("rsmu_cmd", true, &["write smu_args", "write rsmu_cmd", "read rsmu_cmd"]; "status read")]
#[test_case("smu_args", true, &["write smu_args", "write rsmu_cmd", "read rsmu_cmd", "read smu_args"]; "response read")]
fn a_timed_out_sysfs_call_starts_no_later_step_after_driver_drop(
    blocked_file: &'static str,
    block_read: bool,
    expected: &[&str],
) {
    let tree = Tree::new(8, true);
    tree.put_bytes("sys/kernel/ryzen_smu_drv/smn", &[0; 4]);
    tree.put_bytes("sys/kernel/ryzen_smu_drv/smu_args", &[0; 24]);
    tree.put_bytes("sys/kernel/ryzen_smu_drv/rsmu_cmd", &1_u32.to_le_bytes());
    let crate::files::Files::Readers(readers) = tree.source() else {
        panic!("the witness needs in-memory sysfs files");
    };
    let entered = Gate::default();
    let release = Gate::default();
    let (alive, stopped) = mpsc::channel::<()>();
    let steps: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorded = Arc::clone(&steps);
    let (opened, held) = (entered.clone(), release.clone());
    let read_file = Arc::clone(&readers.read);
    let write_file = Arc::clone(&readers.write);
    let read_alive = alive.clone();
    let read_steps = Arc::clone(&steps);
    let (read_entered, read_release) = (entered.clone(), release.clone());
    let mut driver = Driver::from_readers(
        tree.root(),
        readers.cpu_names.clone(),
        true,
        move |path| {
            let _keep_alive = &read_alive;
            let name = path.file_name().unwrap().to_str().unwrap();
            if matches!(name, "smu_args" | "rsmu_cmd") {
                lock(&read_steps).push(format!("read {name}"));
                if block_read && name == blocked_file {
                    read_entered.open();
                    read_release.wait();
                }
            }
            read_file(path)
        },
        move |path, bytes| {
            let _keep_alive = &alive;
            let name = path.file_name().unwrap().to_str().unwrap();
            if matches!(name, "smu_args" | "rsmu_cmd") {
                lock(&recorded).push(format!("write {name}"));
                if !block_read && name == blocked_file {
                    opened.open();
                    held.wait();
                }
                Ok(bytes.len())
            } else {
                write_file(path, bytes)
            }
        },
    )
    .unwrap();
    let _ = driver.validate().unwrap();
    driver.set_deadline(OVERRUN);

    let error = write_all(&mut driver, TargetOffset::clamped(-10)).unwrap_err();
    assert!(matches!(
        smu_error(&error),
        Error::Mailbox {
            source: MailboxError::TimedOut {
                request: Request::Command(Command::SetAllOffsets),
                ..
            },
            ..
        }
    ));
    assert!(entered.wait_timeout(GENEROUS));
    assert_eq!(*lock(&steps), expected);
    poisoned_by(
        &write_all(&mut driver, TargetOffset::ZERO).unwrap_err(),
        Request::Command(Command::SetAllOffsets),
    );

    // The blocked call can complete, but the poisoned worker must not start its next step.
    drop(driver);
    release.open();
    assert!(matches!(
        stopped.recv_timeout(GENEROUS),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
    assert_eq!(*lock(&steps), expected);
}

/// Counts its calls and, on each, opens `entered`, waits for `release`, then yields one byte.
struct StallingRead {
    calls: Arc<AtomicUsize>,
    entered: Gate,
    release: Gate,
}

impl Read for StallingRead {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.open();
        self.release.wait();
        Ok(buffer.first_mut().map_or(0, |slot| {
            *slot = 1;
            1
        }))
    }
}

#[test]
fn a_read_that_returns_partial_data_after_the_timeout_starts_no_further_read() {
    let worker = Worker::spawn(Box::new(Fake::new()), SHORT).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Gate::default();
    let release = Gate::default();
    let reader = StallingRead {
        calls: Arc::clone(&calls),
        entered: entered.clone(),
        release: release.clone(),
    };
    let path = PathBuf::from("stalled");
    let (outcome, received) = mpsc::channel();
    let read_path = path.clone();
    let stuck = Request::Command(Command::ReadOffset);
    let ticket = worker
        .submit(stuck, move |_, poison| {
            let _ = outcome.send(read_gated::<4>(read_path, Ok(reader), poison));
            Ok(())
        })
        .unwrap();
    assert!(
        entered.wait_timeout(GENEROUS),
        "the read never reached the reader"
    );
    assert!(matches!(
        worker.finish(ticket).unwrap_err(),
        MailboxError::TimedOut { request, deadline } if request == stuck && deadline == SHORT
    ));

    release.open();
    let error = received
        .recv_timeout(GENEROUS)
        .expect("the read never finished")
        .unwrap_err();
    let MailboxError::Read {
        path: failed,
        source,
    } = error
    else {
        panic!("expected a read failure, got {error:?}");
    };
    assert_eq!(failed, path);
    let cause = source
        .get_ref()
        .and_then(|cause| cause.downcast_ref::<MailboxError>());
    assert!(
        matches!(cause, Some(MailboxError::Poisoned { request }) if *request == stuck),
        "{cause:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a read started after its caller was told it failed"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn real_fs_a_write_whose_open_returns_after_the_timeout_writes_nothing() {
    let tree = Tree::real(8, true);
    tree.hardware_files();
    let mut driver = Driver::open(tree.root()).unwrap();
    let _ = driver.validate().unwrap();
    driver.set_deadline(OVERRUN);
    let args = format!("{DRIVER_DIR}/smu_args");
    tree.remove(&args);
    let fifo = tree.path(&args);
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();

    // Opening a FIFO for writing blocks until a reader opens it, so the worker is stuck in the
    // kernel inside its open when the timeout is reported.
    let error = write_all(&mut driver, TargetOffset::clamped(-10)).unwrap_err();
    assert!(matches!(
        smu_error(&error),
        Error::Mailbox {
            source: MailboxError::TimedOut {
                request: Request::Command(Command::SetAllOffsets),
                ..
            },
            ..
        }
    ));

    // Opening the read end only now lets the stuck open return after the timeout. A blocking
    // open for reading returns only once the writer is in its open, which is the
    // synchronization; the helper thread keeps a missing writer from hanging the test.
    let (opened, reader) = mpsc::channel();
    thread::spawn(move || {
        let _ = opened.send(File::open(&fifo));
    });
    let mut read_end = reader
        .recv_timeout(GENEROUS)
        .expect("the worker never reached the open")
        .unwrap();
    let mut arrived = Vec::new();
    read_end.read_to_end(&mut arrived).unwrap();
    assert_eq!(arrived, Vec::<u8>::new());
    assert_eq!(
        fs::read(tree.path(&format!("{DRIVER_DIR}/rsmu_cmd"))).unwrap(),
        1_u32.to_le_bytes()
    );
}
