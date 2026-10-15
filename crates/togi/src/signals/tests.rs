//! The registry is process-wide state, so these cases rely on nextest running each test in its own process.

use std::io;
use std::sync::atomic::Ordering;
use std::thread;

use togi_machine::CancellationToken;

use super::{LOST, cancel_all, guarded, listen_after, tokens};

#[test]
fn real_process_a_signal_while_the_listener_starts_still_cancels_the_run() {
    let token = CancellationToken::new();
    let listening = listen_after(&token, || {
        assert_eq!(
            tokens().len(),
            1,
            "the token was not registered before the listener started"
        );
        cancel_all();
        Ok(())
    })
    .unwrap();
    assert!(token.is_cancelled(), "the signal in the gap was lost");
    drop(listening);
}

#[test]
fn real_process_a_listener_that_cannot_start_leaves_no_token_behind() {
    let token = CancellationToken::new();
    let refused = listen_after(&token, || Err(io::Error::other("no signal handlers")));
    assert!(refused.is_err());
    assert!(tokens().is_empty());
}

#[test]
fn real_process_a_listener_that_dies_by_a_panic_cancels_every_registered_run() {
    let token = CancellationToken::new();
    tokens().push((u64::MAX, token.clone()));
    let _ = thread::spawn(|| guarded(|| panic!("the listener broke"))).join();
    assert!(token.is_cancelled(), "a run went on deaf to its signals");
    assert!(LOST.load(Ordering::SeqCst));
    let later = listen_after(&CancellationToken::new(), || Ok(()));
    assert!(later.is_err(), "a later run was given a dead listener");
    assert_eq!(tokens().len(), 1);
}

#[test]
fn real_process_a_listener_that_ends_without_a_panic_cancels_nothing() {
    let token = CancellationToken::new();
    tokens().push((u64::MAX, token.clone()));
    thread::spawn(|| guarded(|| {})).join().unwrap();
    assert!(!token.is_cancelled());
    assert!(!LOST.load(Ordering::SeqCst));
}
