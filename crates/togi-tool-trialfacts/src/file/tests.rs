use std::cell::RefCell;
use std::rc::Rc;

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Create,
    Write,
    Flush,
    Sync,
    Rename,
}

#[derive(Default)]
struct Storage {
    published: Option<Vec<u8>>,
    temporary: Option<Vec<u8>>,
    synced: bool,
    operations: Vec<Operation>,
    fail: Option<Operation>,
}

impl Storage {
    fn step(&mut self, operation: Operation) -> io::Result<()> {
        self.operations.push(operation);
        if self.fail == Some(operation) {
            return Err(io::Error::other(format!("injected {operation:?} failure")));
        }
        Ok(())
    }

    fn read(&self) -> Option<Vec<TrialFact>> {
        self.published
            .as_deref()
            .map(|bytes| decode(bytes).unwrap())
    }
}

struct Temporary(Rc<RefCell<Storage>>);

impl Write for Temporary {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut storage = self.0.borrow_mut();
        // A failed write may have changed the temporary file, never the published one.
        if storage.fail == Some(Operation::Write) {
            storage
                .temporary
                .as_mut()
                .unwrap()
                .extend_from_slice(&bytes[..bytes.len() / 2]);
        }
        storage.step(Operation::Write)?;
        storage.temporary.as_mut().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.borrow_mut().step(Operation::Flush)
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        self.0.borrow_mut().temporary = None;
    }
}

fn publish(storage: &Rc<RefCell<Storage>>, facts: &[TrialFact]) -> Result<(), WriteError> {
    write_with(
        Path::new("/extracts/facts.gz"),
        facts,
        |parent| {
            assert_eq!(parent, Path::new("/extracts"));
            let mut state = storage.borrow_mut();
            state.step(Operation::Create)?;
            state.temporary = Some(Vec::new());
            Ok(Temporary(Rc::clone(storage)))
        },
        |temporary| {
            let mut state = temporary.0.borrow_mut();
            assert_eq!(decode(state.temporary.as_deref().unwrap()).unwrap(), facts);
            state.step(Operation::Sync)?;
            state.synced = true;
            Ok(())
        },
        |temporary, path| {
            assert_eq!(path, Path::new("/extracts/facts.gz"));
            let mut state = temporary.0.borrow_mut();
            assert!(state.synced, "replacement must follow a successful sync");
            state.step(Operation::Rename)?;
            state.published = state.temporary.take();
            Ok(())
        },
    )
}

fn facts() -> Vec<TrialFact> {
    decode(&include_bytes!("../../../../tools/bench/facts/target.jsonl.gz")[..])
        .unwrap()
        .into_iter()
        .take(2)
        .collect()
}

fn encoded(facts: &[TrialFact]) -> Vec<u8> {
    let mut bytes = Vec::new();
    encode(&mut bytes, facts).unwrap();
    bytes
}

#[test]
fn a_complete_extract_is_written_and_synced_before_replacement() {
    let facts = facts();
    let old = &facts[..1];
    let storage = Rc::new(RefCell::new(Storage {
        published: Some(encoded(old)),
        ..Storage::default()
    }));
    publish(&storage, &facts).unwrap();
    let state = storage.borrow();
    assert_eq!(
        state.operations,
        [
            Operation::Create,
            Operation::Write,
            Operation::Flush,
            Operation::Sync,
            Operation::Rename,
        ]
    );
    assert_eq!(state.read().unwrap(), facts);
    assert!(state.temporary.is_none());
}

#[test]
fn a_failure_before_publication_preserves_the_previous_reader_state() {
    let facts = facts();
    for old in [None, Some(encoded(&facts[..1]))] {
        for failure in [
            Operation::Create,
            Operation::Write,
            Operation::Flush,
            Operation::Sync,
            Operation::Rename,
        ] {
            let storage = Rc::new(RefCell::new(Storage {
                published: old.clone(),
                fail: Some(failure),
                ..Storage::default()
            }));
            let before = storage.borrow().read();
            let error = publish(&storage, &facts).unwrap_err();
            match failure {
                Operation::Rename => assert!(matches!(error, WriteError::Replace { .. })),
                Operation::Create | Operation::Write | Operation::Flush | Operation::Sync => {
                    assert!(matches!(error, WriteError::Io(_)));
                }
            }
            let state = storage.borrow();
            assert_eq!(state.read(), before, "after {failure:?}");
            assert_eq!(state.published, old, "after {failure:?}");
            assert!(state.temporary.is_none(), "after {failure:?}");
            if failure != Operation::Rename {
                assert!(!state.operations.contains(&Operation::Rename));
            }
            if matches!(
                failure,
                Operation::Create | Operation::Write | Operation::Flush
            ) {
                assert!(!state.operations.contains(&Operation::Sync));
            }
        }
    }
}

#[test]
fn invalid_facts_do_not_even_create_a_temporary_file() {
    let facts = facts();
    let mut invalid = facts.clone();
    invalid[0].class = crate::Class::Idle {
        loaded: togi_machine::CoreSet::default(),
    };
    invalid[0].outcome = togi_facts::Decisive::Pass;
    let mut not_finite = facts.clone();
    not_finite[0].voltage_requests_v = [(togi_machine::CoreId::new(0).unwrap(), f64::NAN)]
        .into_iter()
        .collect();
    for records in [Vec::new(), invalid, not_finite] {
        let storage = Rc::new(RefCell::new(Storage {
            published: Some(encoded(&facts)),
            ..Storage::default()
        }));
        let before = storage.borrow().read();
        assert!(matches!(
            publish(&storage, &records),
            Err(WriteError::Empty | WriteError::Invalid { .. } | WriteError::NotFinite { .. })
        ));
        let state = storage.borrow();
        assert_eq!(state.read(), before);
        assert!(state.operations.is_empty());
        assert!(state.temporary.is_none());
    }
}

#[test]
fn a_bare_output_name_creates_its_temporary_extract_in_the_current_directory() {
    let facts = facts();
    write_with(
        Path::new("facts.gz"),
        &facts,
        |parent| {
            assert_eq!(parent, Path::new("."));
            Ok(Vec::new())
        },
        |_| Ok(()),
        |bytes, path| {
            assert_eq!(path, Path::new("facts.gz"));
            assert_eq!(decode(bytes.as_slice()).unwrap(), facts);
            Ok(())
        },
    )
    .unwrap();
}
