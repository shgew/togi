//! What a sweep does when SIGINT, SIGTERM or SIGHUP arrives: forward it to the bench while the bench runs, and
//! clean up and exit while the audit reads what the bench left.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rustix::process::{Pid, Signal, kill_process};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

/// The exit status of an interrupted sweep, as the bench exits an interrupted run.
pub const INTERRUPTED: u8 = 1;

#[derive(Debug)]
enum Phase {
    Idle,
    Bench(Pid),
    Audit(Option<PathBuf>),
}

/// Shared between the sweep and the watcher thread. Handlers feed the watcher, so the signal mask stays empty and
/// the bench starts with these signals unblocked.
#[derive(Debug)]
pub struct Interrupt {
    signalled: AtomicBool,
    phase: Mutex<Phase>,
}

/// Held while one directory is audited: a signal waits for it, so nothing is created under the scratch once its
/// removal has begun. Dropping it after a signal arrived removes the scratch and exits.
#[derive(Debug)]
pub struct Holding<'a> {
    interrupt: &'a Interrupt,
    phase: MutexGuard<'a, Phase>,
}

impl Drop for Holding<'_> {
    fn drop(&mut self) {
        if self.interrupt.signalled() {
            if let Phase::Audit(scratch) = &*self.phase {
                conclude(scratch.as_deref());
            }
        }
    }
}

impl Interrupt {
    fn new() -> Self {
        Self {
            signalled: AtomicBool::new(false),
            phase: Mutex::new(Phase::Idle),
        }
    }

    pub fn watch() -> io::Result<Arc<Self>> {
        let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
        let interrupt = Arc::new(Self::new());
        let watcher = Arc::clone(&interrupt);
        std::thread::Builder::new()
            .name("signals".into())
            .spawn(move || {
                for raw in signals.forever() {
                    watcher.deliver(raw);
                }
            })?;
        Ok(interrupt)
    }

    fn lock(&self) -> MutexGuard<'_, Phase> {
        self.phase
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn deliver(&self, raw: i32) {
        self.signalled.store(true, Ordering::SeqCst);
        match &*self.lock() {
            Phase::Idle => {}
            Phase::Bench(pid) => forward(*pid, Signal::from_named_raw(raw)),
            Phase::Audit(scratch) => conclude(scratch.as_deref()),
        }
    }

    pub fn signalled(&self) -> bool {
        self.signalled.load(Ordering::SeqCst)
    }

    /// Starts forwarding signals to `pid`, which this sweep has spawned and not yet reaped, and forwards one that
    /// already arrived.
    pub(crate) fn bench_started(&self, pid: Pid) {
        let mut phase = self.lock();
        *phase = Phase::Bench(pid);
        if self.signalled() {
            forward(pid, Some(Signal::TERM));
        }
    }

    /// Stops forwarding; call before reaping the bench so a signal never reaches a reused process id.
    pub(crate) fn bench_exited(&self) {
        *self.lock() = Phase::Idle;
    }

    /// Makes a signal from now on remove `scratch`, when there is one, and exit; false when one has already arrived.
    pub fn audit_started(&self, scratch: Option<PathBuf>) -> bool {
        let mut phase = self.lock();
        if self.signalled() {
            return false;
        }
        *phase = Phase::Audit(scratch);
        true
    }

    /// Holds off a signal while one directory is audited.
    pub fn hold(&self) -> Holding<'_> {
        Holding {
            interrupt: self,
            phase: self.lock(),
        }
    }
}

fn conclude(scratch: Option<&Path>) {
    if let Some(scratch) = scratch {
        let _ = std::fs::remove_dir_all(scratch);
    }
    let _ = writeln!(io::stderr(), "togi-tool-audit: interrupted");
    std::process::exit(INTERRUPTED.into());
}

fn forward(pid: Pid, signal: Option<Signal>) {
    if let Some(signal) = signal {
        let _ = kill_process(pid, signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_waits_for_the_directory_audit_that_holds_it_off() {
        let interrupt = Arc::new(Interrupt::new());
        let held = interrupt.hold();
        let watcher = Arc::clone(&interrupt);
        let delivery = std::thread::spawn(move || watcher.deliver(SIGTERM));
        while !interrupt.signalled() {
            std::thread::yield_now();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !delivery.is_finished(),
            "the signal was handled during the audit"
        );
        drop(held);
        delivery.join().unwrap();
    }
}
