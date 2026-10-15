//! The hook is process-wide state, so these cases rely on nextest running each test in its own process.

use std::panic::{self, AssertUnwindSafe};
use std::sync::PoisonError;
use std::thread;

use super::{HELD, contained, install, screen_owned};

fn held() -> Vec<String> {
    HELD.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

#[test]
fn real_process_a_panic_on_another_thread_neither_ends_the_process_nor_prints_over_the_dashboard() {
    install();
    screen_owned(true);
    let worker = thread::Builder::new()
        .name("worker".to_owned())
        .spawn(|| panic!("worker bug \x1b[31m"))
        .unwrap();
    assert!(worker.join().is_err(), "the panic unwinds the worker");
    let [line] = held().try_into().unwrap();
    assert!(
        line.starts_with("togi: panic on thread worker: worker bug \\x1b[31m ("),
        "{line:?}"
    );
    screen_owned(false);
    assert!(
        held().is_empty(),
        "the report is printed once the screen is free"
    );
}

#[test]
fn real_process_a_panic_another_thread_catches_itself_leaves_the_process_running_and_togis_own_is_silent()
 {
    install();
    screen_owned(true);
    let worker = thread::spawn(|| {
        panic::catch_unwind(AssertUnwindSafe(|| panic!("caught by its own thread")))
    });
    assert!(worker.join().unwrap().is_err());
    assert_eq!(
        held().len(),
        1,
        "the hook reports it, the thread catches it"
    );
    assert_eq!(
        contained(|| panic!("contained")),
        Err("contained".to_owned())
    );
    assert_eq!(held().len(), 1, "a panic togi contains is not reported");
    screen_owned(false);
}
