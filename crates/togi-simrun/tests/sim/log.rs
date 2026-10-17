use std::cell::{Cell, RefCell};
use std::io::{self, Write};
use std::rc::Rc;

use togi_journal::{Event, EventKind, MemFs};
use togi_session::SessionError;
use togi_simrun::{Observe, Persistence, SimError, SimInput, SimOutcome, events, simulate};

use crate::common::{hunt_machine, input, nz, simulator};

struct Log {
    fail_writes: bool,
    fail_flush: bool,
    flushes: Rc<Cell<u32>>,
}

impl Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.fail_writes {
            return Err(io::Error::other("the log is gone"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes.set(self.flushes.get() + 1);
        if self.fail_flush {
            return Err(io::Error::other("the log cannot flush"));
        }
        Ok(())
    }
}

fn log(fail_writes: bool, fail_flush: bool) -> (Box<dyn Write>, Rc<Cell<u32>>) {
    let flushes = Rc::new(Cell::new(0));
    let log = Log {
        fail_writes,
        fail_flush,
        flushes: Rc::clone(&flushes),
    };
    (Box::new(log), flushes)
}

fn shown(outcome: &Result<SimOutcome, SimError>) -> String {
    match outcome {
        Ok(SimOutcome::Capped { boots }) => format!("capped after {boots} boots"),
        Ok(SimOutcome::Stopped(finished)) => {
            format!("stopped by {:?} with {:?}", finished.stop, finished.errors)
        }
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_log_that_fails_during_a_crashed_boot_fails_the_simulation_instead_of_capping_it() {
    let mut input = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Memory(MemFs::new()),
    );
    input.max_boots = nz(3);
    input.log = Some(log(true, false).0);
    match simulate(input) {
        Err(SimError::Failed { boot: 1, errors }) => {
            assert!(
                matches!(errors.as_slice(), [SessionError::Log(_)]),
                "{errors:?}"
            );
            assert_eq!(
                togi_machine::error_chain(&SimError::Failed { boot: 1, errors }),
                "boot 1 crashed and also failed: write the event log: the log is gone"
            );
        }
        other => panic!("the log failure was lost: {}", shown(&other)),
    }
}

#[test]
fn a_log_that_cannot_flush_fails_the_simulation() {
    let mut input = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Memory(MemFs::new()),
    );
    input.max_boots = nz(3);
    let (log, flushes) = log(false, true);
    input.log = Some(log);
    let outcome = simulate(input);
    assert_eq!(flushes.get(), 1);
    assert!(
        matches!(outcome, Err(SimError::FlushLog(_))),
        "the flush failure was lost: {}",
        shown(&outcome)
    );
}

#[test]
fn an_observer_sees_every_append_after_the_log_fails() {
    let mut input = input(
        simulator(&hunt_machine(2), 1),
        Persistence::Memory(MemFs::new()),
    );
    input.log = Some(log(true, false).0);
    let seen = Rc::new(Cell::new(0));
    let counted = Rc::clone(&seen);
    input.observer = Some(Box::new(move |event| {
        if event.kind() != EventKind::TrialEnd {
            return Observe::Continue;
        }
        counted.set(counted.get() + 1);
        Observe::Stop
    }));
    let outcome = simulate(input);
    assert_eq!(seen.get(), 1, "{}", shown(&outcome));
    let Ok(SimOutcome::Stopped(finished)) = outcome else {
        panic!("not stopped by the observer: {}", shown(&outcome));
    };
    assert!(
        finished
            .errors
            .iter()
            .any(|error| matches!(error, SessionError::Log(_))),
        "{:?}",
        finished.errors
    );
}

fn recording(input: &mut SimInput) -> Rc<RefCell<Vec<Event>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    input.observer = Some(Box::new(move |event| {
        sink.borrow_mut().push(event.clone());
        Observe::Continue
    }));
    seen
}

fn observed_run_equals_journal(
    persistence: &Persistence,
    log: Option<Box<dyn Write>>,
    replay: togi_simrun::Replay,
) {
    let mut input = input(simulator(&hunt_machine(2), 1), persistence.clone());
    input.log = log;
    input.replay = replay;
    let seen = recording(&mut input);
    let outcome = simulate(input);
    assert!(
        matches!(outcome, Ok(SimOutcome::Stopped(_))),
        "{}",
        shown(&outcome)
    );
    let journal = events(persistence).unwrap();
    assert!(
        journal
            .iter()
            .any(|event| event.kind() == EventKind::CrashDetected),
        "the run never crashed, so it never crossed a boot"
    );
    assert_eq!(*seen.borrow(), journal);
}

#[test]
fn an_observer_sees_the_journal_in_order_across_boots() {
    observed_run_equals_journal(
        &Persistence::Memory(MemFs::new()),
        None,
        togi_simrun::Replay::Warm,
    );
}

#[test]
fn an_observer_sees_the_journal_in_order_beside_a_log() {
    observed_run_equals_journal(
        &Persistence::Memory(MemFs::new()),
        Some(log(false, false).0),
        togi_simrun::Replay::Warm,
    );
}

#[test]
fn an_observer_sees_the_journal_in_order_beside_a_failing_log() {
    let persistence = Persistence::Memory(MemFs::new());
    let mut input = input(simulator(&hunt_machine(2), 1), persistence.clone());
    input.max_boots = nz(3);
    input.log = Some(log(true, false).0);
    let seen = recording(&mut input);
    assert!(simulate(input).is_err());
    let journal = events(&persistence).unwrap();
    assert!(!journal.is_empty());
    assert_eq!(*seen.borrow(), journal);
}

#[test]
fn an_observer_sees_the_journal_in_order_across_cold_boots() {
    observed_run_equals_journal(
        &Persistence::Memory(MemFs::new()),
        Some(log(false, false).0),
        togi_simrun::Replay::Cold,
    );
}

#[test]
fn a_stopped_observer_is_not_called_again_and_the_run_is_cancelled_once() {
    let persistence = Persistence::Memory(MemFs::new());
    let mut input = input(simulator(&hunt_machine(2), 1), persistence.clone());
    let calls = Rc::new(Cell::new(0_u32));
    let counted = Rc::clone(&calls);
    input.observer = Some(Box::new(move |event| {
        counted.set(counted.get() + 1);
        if event.kind() == EventKind::TrialStart {
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    let outcome = simulate(input);
    let Ok(SimOutcome::Stopped(finished)) = outcome else {
        panic!("not stopped: {}", shown(&outcome));
    };
    assert_eq!(finished.stop, Some(togi_session::Stop::Signal));
    let journal = events(&persistence).unwrap();
    let first = journal
        .iter()
        .position(|event| event.kind() == EventKind::TrialStart)
        .unwrap();
    assert_eq!(calls.get() as usize, first + 1);
    assert!(
        journal.len() > first + 1,
        "the run appended nothing after the stop"
    );
}
