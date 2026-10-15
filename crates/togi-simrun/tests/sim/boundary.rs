use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use togi_journal::{EventKind, MemFs};
use togi_session::{SessionError, Stop};
use togi_simrun::{HistoryError, Observe, Persistence, SimError, SimOutcome, events, simulate};

use crate::common::{Bench, STATE, finish, hunt_machine, input, nz, simulator, until_kind};

/// A log that keeps what it is given. With `fail` set it refuses the write that completes a `journal.torn` line's
/// kind, the way a full disk would refuse the repair line.
struct Capture {
    written: Rc<RefCell<Vec<u8>>>,
    fail: bool,
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut written = self.written.borrow_mut();
        written.extend_from_slice(bytes);
        if self.fail && String::from_utf8_lossy(&written).contains("journal.torn") {
            return Err(io::Error::other("the log is gone"));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn captured(fail: bool) -> (Box<dyn Write>, Rc<RefCell<Vec<u8>>>) {
    let written = Rc::new(RefCell::new(Vec::new()));
    let log = Capture {
        written: Rc::clone(&written),
        fail,
    };
    (Box::new(log), written)
}

fn lines(written: &Rc<RefCell<Vec<u8>>>) -> Vec<String> {
    String::from_utf8(written.borrow().clone())
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn torn_lines(written: &Rc<RefCell<Vec<u8>>>) -> usize {
    lines(written)
        .iter()
        .filter(|line| line.contains("journal.torn"))
        .count()
}

/// A bench whose journal stopped after its first trial, with the writer's last record cut short.
fn torn_bench() -> Bench {
    let definition = hunt_machine(2);
    let first = Bench::new(&definition, 1);
    let mut input = first.input();
    input.observer = Some(until_kind(EventKind::TrialEnd));
    assert_eq!(finish(input).stop, Some(Stop::Signal));
    let path = format!("{STATE}/events.jsonl");
    let mut bytes = first.fs.contents(&path).unwrap();
    bytes.extend_from_slice(b"{\"seq\":99999,\"tim");
    first.fs.put(&path, bytes);
    first.resumed(&definition, 1)
}

#[test]
fn resuming_a_torn_journal_logs_its_repair_once() {
    let bench = torn_bench();
    let mut input = bench.input();
    let (log, written) = captured(false);
    input.log = Some(log);
    input.observer = Some(until_kind(EventKind::TrialEnd));
    let finished = finish(input);
    assert!(finished.errors.is_empty(), "{:?}", finished.errors);
    assert_eq!(torn_lines(&written), 1, "{:?}", lines(&written));
    let repair = lines(&written)
        .into_iter()
        .find(|line| line.contains("journal.torn"))
        .unwrap();
    assert!(repair.contains("torn bytes"), "{repair}");
    let journaled = bench
        .events()
        .iter()
        .filter(|event| event.kind() == EventKind::JournalTorn)
        .count();
    assert_eq!(journaled, 1);
}

#[test]
fn a_log_that_refuses_the_repair_line_is_reported_and_the_observer_still_sees_the_run() {
    let bench = torn_bench();
    let mut input = bench.input();
    let (log, written) = captured(true);
    input.log = Some(log);
    let seen = Rc::new(RefCell::new(0));
    let counted = Rc::clone(&seen);
    input.observer = Some(Box::new(move |event| {
        if event.kind() != EventKind::TrialEnd {
            return Observe::Continue;
        }
        *counted.borrow_mut() += 1;
        Observe::Stop
    }));
    let finished = finish(input);
    assert_eq!(*seen.borrow(), 1);
    assert!(
        matches!(finished.errors.as_slice(), [SessionError::Log(_)]),
        "{:?}",
        finished.errors
    );
    // The refused repair line is the last thing the dropped log was given.
    assert_eq!(torn_lines(&written), 1, "{:?}", lines(&written));
    assert_eq!(lines(&written).len(), 1, "{:?}", lines(&written));
}

fn shown(outcome: &Result<SimOutcome, SimError>) -> String {
    match outcome {
        Ok(SimOutcome::Capped { boots }) => format!("capped after {boots} boots"),
        Ok(SimOutcome::Stopped(finished)) => format!("stopped by {:?}", finished.stop),
        Err(error) => error.to_string(),
    }
}

#[test]
fn samples_without_a_state_directory_on_disk_are_refused_before_anything_is_touched() {
    let fs = MemFs::new();
    let sim = simulator(&hunt_machine(2), 1);
    let before = sim.persisted();
    let mut input = input(sim.clone(), Persistence::Memory(fs.clone()));
    input.write_samples = true;
    let outcome = simulate(input);
    assert!(
        matches!(outcome, Err(SimError::SamplesInMemory)),
        "{}",
        shown(&outcome)
    );
    assert!(fs.entries().is_empty(), "{:?}", fs.entries());
    assert_eq!(sim.persisted(), before);
}

#[test]
fn a_machine_crashed_before_the_simulation_fails_boot_one_on_the_host() {
    let bench = Bench::new(&hunt_machine(2), 1);
    bench.sim.crash();
    let before = bench.sim.persisted();
    let outcome = simulate(bench.input());
    assert!(
        matches!(
            outcome,
            Err(SimError::Host {
                boot: 1,
                source: togi_machine::Error::Crashed
            })
        ),
        "{}",
        shown(&outcome)
    );
    assert!(bench.fs.entries().is_empty(), "{:?}", bench.fs.entries());
    assert_eq!(bench.sim.persisted(), before);
}

#[test]
fn real_fs_a_dump_writes_the_capped_memory_directory_to_disk() {
    let bench = Bench::new(&hunt_machine(2), 1);
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("dump");
    let mut input = bench.input();
    input.max_boots = nz(2);
    input.dump_to = Some(target.clone());
    let outcome = simulate(input);
    assert!(
        matches!(outcome, Ok(SimOutcome::Capped { boots }) if boots.get() == 2),
        "{}",
        shown(&outcome)
    );
    let machine = std::fs::read(target.join("state/machine.json")).unwrap();
    assert!(!machine.is_empty());
    assert_eq!(
        machine,
        bench.fs.contents(format!("{STATE}/machine.json")).unwrap()
    );
    assert_eq!(
        std::fs::read(target.join("state/state.json")).unwrap(),
        bench.fs.contents(format!("{STATE}/state.json")).unwrap()
    );
    assert_eq!(
        std::fs::read(target.join("state/events.jsonl")).unwrap(),
        bench.fs.contents(format!("{STATE}/events.jsonl")).unwrap()
    );
}

#[test]
fn a_live_journal_that_was_never_written_has_no_events() {
    assert!(
        events(&Persistence::Memory(MemFs::new()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_live_journal_is_read_back() {
    let fs = MemFs::new();
    let persistence = Persistence::Memory(fs.clone());
    let mut input = input(simulator(&hunt_machine(2), 1), persistence.clone());
    input.max_boots = nz(2);
    let outcome = simulate(input);
    assert!(
        matches!(outcome, Ok(SimOutcome::Capped { .. })),
        "{}",
        shown(&outcome)
    );
    let read = events(&persistence).unwrap();
    let journal = String::from_utf8(fs.contents(format!("{STATE}/events.jsonl")).unwrap()).unwrap();
    assert!(read.len() > 2);
    assert_eq!(read.len(), journal.lines().count());
    assert_eq!(read[0].kind(), EventKind::SessionStart);
}

#[test]
fn a_malformed_live_journal_is_an_error_not_an_empty_history() {
    let fs = MemFs::new();
    fs.put(format!("{STATE}/events.jsonl"), "garbage\n");
    assert!(matches!(
        events(&Persistence::Memory(fs)),
        Err(HistoryError::Read(_))
    ));
}

#[test]
fn archives_are_not_part_of_the_live_events() {
    let bench = Bench::new(&hunt_machine(2), 1);
    let mut input = bench.input();
    input.observer = Some(until_kind(EventKind::TrialEnd));
    finish(input);
    let live = bench.events();
    assert!(!live.is_empty());
    bench.fs.put(
        format!("{STATE}/archive/20231113T000000Z.jsonl"),
        "garbage\n",
    );
    assert_eq!(bench.events(), live);

    let archived_only = MemFs::new();
    archived_only.put(
        format!("{STATE}/archive/20231113T000000Z.jsonl"),
        "garbage\n",
    );
    assert!(
        events(&Persistence::Memory(archived_only))
            .unwrap()
            .is_empty()
    );
}
