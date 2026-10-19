use std::thread;
use std::time::{Duration, Instant};

use togi_machine::{CancellationToken, Clock, Error};

use crate::adapters::SystemClock;

#[test]
fn a_cancelled_sleep_returns_cancelled_at_once() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let begun = Instant::now();
    let result = SystemClock.sleep(&cancel, Duration::from_secs(3600));
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert!(begun.elapsed() < Duration::from_secs(5));
}

#[test]
fn cancelling_during_a_sleep_wakes_it() {
    let cancel = CancellationToken::new();
    let waker = cancel.clone();
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        waker.cancel();
    });
    let begun = Instant::now();
    let result = SystemClock.sleep(&cancel, Duration::from_secs(3600));
    handle.join().unwrap();
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert!(begun.elapsed() < Duration::from_secs(30));
}

#[test]
fn an_uncancelled_sleep_lasts_its_duration() {
    let cancel = CancellationToken::new();
    let sleep = Duration::from_millis(40);
    let begun = SystemClock.monotonic().unwrap();
    SystemClock.sleep(&cancel, sleep).unwrap();
    assert!(SystemClock.monotonic().unwrap().checked_sub(begun).unwrap() >= sleep);
}

#[test]
fn the_monotonic_clock_counts_time_since_boot() {
    let first = SystemClock.monotonic().unwrap();
    let second = SystemClock.monotonic().unwrap();
    assert!(first > Duration::ZERO);
    assert!(second >= first);
}
