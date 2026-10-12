//! What a sweep does when SIGINT, SIGTERM or SIGHUP arrives: forward it to the bench while the bench runs, and
//! clean up and exit while the audit reads what the bench left.

use std::io::{self, Write as _};
use std::path::PathBuf;
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

#[derive(Debug)]
struct State {
    signalled: bool,
    phase: Phase,
}

/// Shared between the sweep and the watcher thread. Handlers feed the watcher, so the signal mask stays empty and
/// the bench starts with these signals unblocked.
#[derive(Debug)]
pub struct Interrupt {
    state: Mutex<State>,
}

impl Interrupt {
    pub fn watch() -> io::Result<Arc<Self>> {
        let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
        let interrupt = Arc::new(Self {
            state: Mutex::new(State {
                signalled: false,
                phase: Phase::Idle,
            }),
        });
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

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn deliver(&self, raw: i32) {
        let mut state = self.lock();
        state.signalled = true;
        match &state.phase {
            Phase::Idle => {}
            Phase::Bench(pid) => forward(*pid, Signal::from_named_raw(raw)),
            Phase::Audit(scratch) => {
                if let Some(scratch) = scratch {
                    let _ = std::fs::remove_dir_all(scratch);
                }
                let _ = writeln!(io::stderr(), "togi-tool-audit: interrupted");
                std::process::exit(INTERRUPTED.into());
            }
        }
    }

    pub fn signalled(&self) -> bool {
        self.lock().signalled
    }

    /// Starts forwarding signals to `pid`, which this sweep has spawned and not yet reaped, and forwards one that
    /// already arrived.
    pub(crate) fn bench_started(&self, pid: Pid) {
        let mut state = self.lock();
        state.phase = Phase::Bench(pid);
        if state.signalled {
            forward(pid, Some(Signal::TERM));
        }
    }

    /// Stops forwarding; call before reaping the bench so a signal never reaches a reused process id.
    pub(crate) fn bench_exited(&self) {
        self.lock().phase = Phase::Idle;
    }

    /// Makes a signal from now on remove `scratch`, when there is one, and exit; false when one has already arrived.
    pub fn audit_started(&self, scratch: Option<PathBuf>) -> bool {
        let mut state = self.lock();
        if state.signalled {
            return false;
        }
        state.phase = Phase::Audit(scratch);
        true
    }
}

fn forward(pid: Pid, signal: Option<Signal>) {
    if let Some(signal) = signal {
        let _ = kill_process(pid, signal);
    }
}
