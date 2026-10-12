use std::io;
use std::thread;

use nix::sys::signal::{SigSet, Signal};
use togi_machine::CancellationToken;

/// Cancels `cancel` on SIGINT, SIGTERM and SIGHUP. The signals are blocked in the calling thread, which threads
/// spawned later inherit, and a dedicated thread waits for them, so no handler runs inside the session.
pub(crate) fn cancel_on_signals(cancel: CancellationToken) -> io::Result<()> {
    let mut set = SigSet::empty();
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        set.add(signal);
    }
    set.thread_block().map_err(io::Error::from)?;
    thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            while set.wait().is_ok() {
                cancel.cancel();
            }
        })
        .map(drop)
}
