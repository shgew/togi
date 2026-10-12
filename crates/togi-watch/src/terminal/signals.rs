//! Signals: SIGINT, SIGTERM and SIGHUP cancel the dashboard, SIGWINCH redraws it.
//!
//! Each handler only writes the signal number to a pipe, which the loop reads. Installing a handler saves the
//! action that was there, and dropping the [`Installed`] guard puts that action back exactly, on every exit and
//! on a panic, so a signal sent after the dashboard is gone does what it did before it.

use std::io;
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use nix::errno::Errno;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use rustix::io::Errno as IoErrno;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use super::Event;

/// The write end of the pipe the handler writes to, or -1 while no handler is installed.
static HANDLER_PIPE: AtomicI32 = AtomicI32::new(-1);

/// The signals' pipe, made once for the process and handed to one dashboard at a time.
static PIPE: Mutex<Option<(OwnedFd, OwnedFd)>> = Mutex::new(None);

#[expect(
    unsafe_code,
    reason = "a signal handler is an extern function; it only makes the async-signal-safe calls write and errno"
)]
extern "C" fn handle(number: nix::libc::c_int) {
    let errno = Errno::last_raw();
    let fd = HANDLER_PIPE.load(Ordering::Relaxed);
    if fd >= 0 {
        // SAFETY: the descriptor is the write end of the process's signal pipe, which is never closed once made.
        let fd = unsafe { BorrowedFd::borrow_raw(fd) };
        let _ = rustix::io::write(fd, &[u8::try_from(number).unwrap_or(0)]);
    }
    Errno::set_raw(errno);
}

/// The handlers of one dashboard. Dropping it restores every action it replaced.
pub(super) struct Installed {
    previous: Vec<(Signal, SigAction)>,
    pipe: MutexGuard<'static, Option<(OwnedFd, OwnedFd)>>,
}

fn nonblocking_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let (read, write) = super::input::pipe()?;
    for end in [&read, &write] {
        rustix::fs::fcntl_setfl(end, rustix::fs::OFlags::NONBLOCK)?;
    }
    Ok((read, write))
}

/// Installs the handlers: SIGWINCH always, and SIGINT, SIGTERM and SIGHUP when `terminating`. Only one dashboard
/// at a time can own them, so a second caller waits for the first to finish.
pub(super) fn install(terminating: bool) -> io::Result<Installed> {
    let mut pipe = PIPE.lock().unwrap_or_else(PoisonError::into_inner);
    if pipe.is_none() {
        *pipe = Some(nonblocking_pipe()?);
    }
    let Some((read, write)) = pipe.as_ref() else {
        return Err(io::Error::other("no signal pipe"));
    };
    let mut buf = [0_u8; 64];
    while rustix::io::read(read, &mut buf).is_ok_and(|read| read > 0) {}
    HANDLER_PIPE.store(std::os::fd::AsRawFd::as_raw_fd(write), Ordering::Relaxed);
    let mut installed = Installed {
        previous: Vec::new(),
        pipe,
    };
    let wanted: &[Signal] = if terminating {
        &[
            Signal::SIGINT,
            Signal::SIGTERM,
            Signal::SIGHUP,
            Signal::SIGWINCH,
        ]
    } else {
        &[Signal::SIGWINCH]
    };
    for signal in wanted {
        let action = SigAction::new(
            SigHandler::Handler(handle),
            SaFlags::SA_RESTART,
            SigSet::empty(),
        );
        // SAFETY: the handler only writes to a pipe and keeps errno, so it is async-signal-safe; the action it
        // replaces is saved and put back when the guard is dropped.
        #[expect(unsafe_code, reason = "installing a signal handler")]
        let previous = unsafe { sigaction(*signal, &action) }.map_err(io::Error::from)?;
        installed.previous.push((*signal, previous));
    }
    Ok(installed)
}

impl Drop for Installed {
    fn drop(&mut self) {
        for (signal, previous) in self.previous.drain(..).rev() {
            // SAFETY: the action was returned by sigaction for this signal when the handler replaced it.
            #[expect(unsafe_code, reason = "restoring the signal action that was replaced")]
            let _ = unsafe { sigaction(signal, &previous) };
        }
        HANDLER_PIPE.store(-1, Ordering::Relaxed);
    }
}

impl Installed {
    /// Forwards the signals that arrive as events, from a task that ends when the pipe does or the receiver is
    /// gone. It is aborted by its owner.
    pub(super) fn listen(&self, events: UnboundedSender<Event>) -> io::Result<JoinHandle<()>> {
        let Some((read, _)) = self.pipe.as_ref() else {
            return Err(io::Error::other("no signal pipe"));
        };
        let read = AsyncFd::new(read.as_fd().try_clone_to_owned()?)?;
        Ok(tokio::spawn(async move {
            let mut buf = [0_u8; 64];
            loop {
                let Ok(mut ready) = read.readable().await else {
                    return;
                };
                loop {
                    match rustix::io::read(read.get_ref(), &mut buf) {
                        Ok(0) => return,
                        Ok(count) => {
                            for number in &buf[..count] {
                                let event = if i32::from(*number) == Signal::SIGWINCH as i32 {
                                    Event::Resize
                                } else {
                                    Event::Cancel
                                };
                                if events.send(event).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(IoErrno::INTR) => {}
                        Err(IoErrno::AGAIN) => {
                            ready.clear_ready();
                            break;
                        }
                        Err(_) => return,
                    }
                }
            }
        }))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use nix::sys::signal::{SigAction, SigHandler, SigSet, Signal, sigaction};

    use std::sync::{Mutex, MutexGuard, PoisonError};

    use super::install;

    static SERIAL: Mutex<()> = Mutex::new(());

    /// Held by every test that installs the handlers or reads the actions, so that under plain `cargo test`'s
    /// threads one never sees another's. (`cargo nextest` runs each test in its own process.)
    pub(in crate::terminal) fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What now happens on a signal, found by replacing the action with the default and putting it back. The
    /// kernel's own bookkeeping bits of an action (its restorer and the mask libc fills in) differ between reads,
    /// so only the handler and whether it restarts system calls are compared.
    pub(in crate::terminal) fn disposition(signal: Signal) -> (SigHandler, bool) {
        let default = SigAction::new(
            SigHandler::SigDfl,
            nix::sys::signal::SaFlags::empty(),
            SigSet::empty(),
        );
        #[expect(unsafe_code, reason = "reading a signal action takes a sigaction call")]
        // SAFETY: the action is put back at once, and it is the one sigaction returned.
        unsafe {
            let current = sigaction(signal, &default).unwrap();
            sigaction(signal, &current).unwrap();
            (
                current.handler(),
                current
                    .flags()
                    .contains(nix::sys::signal::SaFlags::SA_RESTART),
            )
        }
    }

    const ALL: [Signal; 4] = [
        Signal::SIGINT,
        Signal::SIGTERM,
        Signal::SIGHUP,
        Signal::SIGWINCH,
    ];

    #[test]
    fn every_installed_action_is_put_back_exactly_when_the_guard_drops() {
        let _serial = serial();
        let before = ALL.map(disposition);
        {
            let _installed = install(true).unwrap();
            for signal in ALL {
                assert!(
                    matches!(disposition(signal).0, SigHandler::Handler(_)),
                    "{signal:?} has no handler while the dashboard shows"
                );
            }
        }
        assert_eq!(ALL.map(disposition), before);
    }

    #[test]
    fn a_dashboard_that_does_not_terminate_installs_only_sigwinch() {
        let _serial = serial();
        let before = ALL.map(disposition);
        {
            let _installed = install(false).unwrap();
            for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
                assert_eq!(
                    disposition(signal),
                    before[ALL.iter().position(|s| *s == signal).unwrap()]
                );
            }
            assert!(matches!(
                disposition(Signal::SIGWINCH).0,
                SigHandler::Handler(_)
            ));
        }
        assert_eq!(ALL.map(disposition), before);
    }

    #[test]
    fn a_custom_action_that_was_there_before_is_what_comes_back() {
        extern "C" fn custom(_: nix::libc::c_int) {}
        let _serial = serial();
        let mine = SigAction::new(
            SigHandler::Handler(custom),
            nix::sys::signal::SaFlags::SA_RESTART,
            SigSet::empty(),
        );
        // SAFETY: the handler does nothing.
        #[expect(unsafe_code, reason = "installing a signal handler")]
        let original = unsafe { sigaction(Signal::SIGHUP, &mine) }.unwrap();
        let installed = install(true).unwrap();
        drop(installed);
        assert_eq!(disposition(Signal::SIGHUP), (mine.handler(), true));
        // SAFETY: the action is the one sigaction returned.
        #[expect(unsafe_code, reason = "restoring a signal action")]
        unsafe {
            sigaction(Signal::SIGHUP, &original).unwrap();
        }
    }

    #[test]
    fn the_guard_restores_the_actions_when_a_panic_unwinds_through_it() {
        let _serial = serial();
        let before = ALL.map(disposition);
        let result = std::panic::catch_unwind(|| {
            let _installed = install(true).unwrap();
            panic!("session panic");
        });
        assert!(result.is_err());
        assert_eq!(ALL.map(disposition), before);
    }
}
