use std::cell::Cell;
use std::io::{self, Write};
use std::rc::Rc;

use togi_journal::{EventKind, MemFs};
use togi_session::SessionError;
use togi_simrun::{Observe, Persistence, SimError, SimOutcome, simulate};

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
        Err(SimError::Failed { boot: 1, errors }) => assert!(
            matches!(errors.as_slice(), [SessionError::Log(_)]),
            "{errors:?}"
        ),
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
