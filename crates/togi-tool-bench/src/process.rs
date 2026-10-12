use std::collections::BTreeSet;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};

use crate::error::RunError;

/// The simulators a bench has running, and whether the bench was told to stop or gave up.
///
/// Each simulator leads a process group of its own, so a terminal's signal reaches only the bench and every
/// signal the fleet sends reaches the simulator and what it started. A running simulator is registered from its
/// spawn until it has exited and before it is reaped, so a signal sent to a registered group never reaches a
/// reused process id.
#[derive(Debug, Default)]
pub struct Fleet {
    cancelled: AtomicBool,
    aborted: AtomicBool,
    live: Mutex<BTreeSet<i32>>,
}

impl Fleet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub(crate) fn stopping(&self) -> bool {
        self.cancelled() || self.aborted.load(Ordering::SeqCst)
    }

    /// Stops new runs and asks every running simulator to stop, as an interrupt of it would.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.terminate();
    }

    /// Stops new runs and running simulators after a run failed, so the failure is what the bench reports.
    pub(crate) fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        self.terminate();
    }

    fn terminate(&self) {
        for &raw in &*self.live() {
            send(raw, Signal::TERM);
        }
    }

    fn live(&self) -> MutexGuard<'_, BTreeSet<i32>> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn send(raw: i32, signal: Signal) {
    if let Some(pid) = Pid::from_raw(raw) {
        let _ = kill_process_group(pid, signal);
    }
}

/// How a finished child exited and how long it ran.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Finished {
    /// The exit status, or -1 when a signal ended it.
    pub exit: i32,
    pub wall: Duration,
}

/// Runs `command` in a process group of its own to its end, killing the group when it outlives `limit` and
/// stopping it when `fleet` is cancelled or aborted. No thread polls: the watchdog sleeps on a channel the
/// waiter closes once the child has exited.
#[expect(
    clippy::disallowed_methods,
    reason = "the bench's one call site that spawns the simulator"
)]
pub(crate) fn run(
    fleet: &Fleet,
    command: &mut Command,
    limit: Duration,
) -> Result<Finished, RunError> {
    if fleet.stopping() {
        return Err(RunError::Interrupted);
    }
    let started = Instant::now();
    let mut child = command.process_group(0).spawn().map_err(RunError::Start)?;
    let pid = Pid::from_child(&child);
    let raw = pid.as_raw_nonzero().get();
    fleet.live().insert(raw);
    if fleet.stopping() {
        send(raw, Signal::TERM);
    }
    let timed_out = AtomicBool::new(false);
    let (done, finished) = mpsc::channel::<()>();
    let flag = &timed_out;
    let waited = thread::scope(|scope| {
        scope.spawn(move || {
            if finished.recv_timeout(limit) == Err(RecvTimeoutError::Timeout) {
                let live = fleet.live();
                if live.contains(&raw) {
                    flag.store(true, Ordering::SeqCst);
                    send(raw, Signal::KILL);
                }
            }
        });
        let waited = waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        );
        fleet.live().remove(&raw);
        drop(done);
        waited
    });
    let status = child.wait().map_err(RunError::Wait)?;
    waited.map_err(|errno| RunError::Wait(io::Error::from(errno)))?;
    if timed_out.load(Ordering::SeqCst) {
        return Err(RunError::TimedOut {
            seconds: limit.as_secs(),
        });
    }
    if fleet.stopping() {
        return Err(RunError::Interrupted);
    }
    Ok(Finished {
        exit: status.code().unwrap_or(-1),
        wall: started.elapsed(),
    })
}
