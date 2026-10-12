//! The dashboard `run` shows on stderr while a session runs, and the owner that starts and ends it.
//!
//! The owner draws through a [`Surface`] on its own thread, quiets the terminal's input while it shows
//! (echo and line buffering off, signal keys untouched, so Ctrl-C still reaches `run`'s own listener), and
//! puts everything back however it ends: [`Dashboard::hide`], a drop while a panic unwinds, or the surface
//! failing or panicking, in which case `run` falls back to event lines and the session goes on.

use std::io::{self, Write};
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use rustix::termios::{
    LocalModes, OptionalActions, QueueSelector, Termios, tcflush, tcgetattr, tcsetattr,
};
use togi_render::escape_text;

use crate::panics;

/// Ends a dashboard that is drawing, now or as soon as it starts.
#[derive(Clone, Debug, Default)]
pub struct Stop(Arc<(Mutex<bool>, Condvar)>);

impl Stop {
    /// Ends the dashboard.
    pub fn stop(&self) {
        let (stopped, wake) = &*self.0;
        *stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        wake.notify_all();
    }

    /// Whether the dashboard has been ended.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        *self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Blocks until the dashboard is ended.
    pub fn wait(&self) {
        let (stopped, wake) = &*self.0;
        let mut stopped = stopped.lock().unwrap_or_else(PoisonError::into_inner);
        while !*stopped {
            stopped = wake.wait(stopped).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Draws the dashboard of a journal.
pub trait Surface: Send + Sync {
    /// Shows the dashboard of the journal in `state_dir` on stderr until `stop` ends it. Until the journal holds an
    /// event after sequence number `since`, it says the run is starting.
    ///
    /// # Errors
    ///
    /// Why it cannot show or stopped showing.
    fn draw(&self, state_dir: &Path, since: u64, stop: &Stop) -> Result<(), String>;
}

/// The `watch` dashboard of `togi-watch`, drawn on stderr. It takes no keys, and listens only for SIGWINCH:
/// `run` keeps its own listener for the signals that stop it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Watch;

impl Surface for Watch {
    fn draw(&self, state_dir: &Path, since: u64, stop: &Stop) -> Result<(), String> {
        let ended = togi_watch::terminal::Stop::new();
        draw_until(stop, &ended, || {
            togi_watch::terminal::run_after(state_dir, since, &ended, rustix::stdio::stderr(), None)
        })
        .map_err(|error| error.to_string())
    }
}

/// Runs `draw`, which ends when `ended` does, while a helper ends `ended` once `stop` is stopped. `stop` is
/// stopped when `draw` returns or panics: a panic unwinds into the scope, which joins the helper before it
/// goes on, so the helper must not wait for a stop only a later `hide` would give.
fn draw_until<T>(stop: &Stop, ended: &togi_watch::terminal::Stop, draw: impl FnOnce() -> T) -> T {
    struct Release<'a>(&'a Stop);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            self.0.stop();
        }
    }
    thread::scope(|scope| {
        scope.spawn(|| {
            stop.wait();
            ended.stop();
        });
        let _release = Release(stop);
        draw()
    })
}

/// The terminal's input with echo and line buffering off. Dropping it puts the previous settings back.
struct QuietInput<'a> {
    fd: BorrowedFd<'a>,
    saved: Termios,
}

impl<'a> QuietInput<'a> {
    /// Turns echo and line buffering off on `fd`, leaving its signal keys on; `None` when it is not a terminal.
    fn enter(fd: BorrowedFd<'a>) -> io::Result<Option<Self>> {
        let Ok(saved) = tcgetattr(fd) else {
            return Ok(None);
        };
        let mut quiet = saved.clone();
        quiet.local_modes &= !(LocalModes::ECHO | LocalModes::ICANON);
        tcsetattr(fd, OptionalActions::Flush, &quiet)?;
        Ok(Some(Self { fd, saved }))
    }
}

impl Drop for QuietInput<'_> {
    fn drop(&mut self) {
        let _ = tcsetattr(self.fd, OptionalActions::Flush, &self.saved);
    }
}

/// Drops what typed ahead of a question, so only an answer typed after it counts.
pub fn discard_typed_ahead() {
    discard_on(rustix::stdio::stdin());
}

fn discard_on(fd: BorrowedFd<'_>) {
    let _ = tcflush(fd, QueueSelector::IFlush);
}

struct Showing {
    stop: Stop,
    thread: JoinHandle<()>,
    quiet: Option<QuietInput<'static>>,
}

type Notice = Arc<dyn Fn(&str) + Send + Sync>;

/// Says on stderr that the dashboard gave way to event lines.
fn print_fallback(message: &str) {
    let _ = writeln!(
        io::stderr(),
        "togi run: dashboard: {}; printing events instead",
        escape_text(message)
    );
}

/// Starts the dashboard and ends it on every exit.
pub struct Dashboard {
    surface: Arc<dyn Surface>,
    state_dir: PathBuf,
    since: u64,
    visible: Arc<AtomicBool>,
    showing: Mutex<Option<Showing>>,
    notice: Notice,
}

impl Dashboard {
    /// A dashboard of the journal in `state_dir` that says the run is starting until an event after `since`.
    #[must_use]
    pub fn new(surface: Arc<dyn Surface>, state_dir: &Path, since: u64) -> Self {
        Self {
            surface,
            state_dir: state_dir.to_owned(),
            since,
            visible: Arc::new(AtomicBool::new(false)),
            showing: Mutex::new(None),
            notice: Arc::new(print_fallback),
        }
    }

    /// A writer that drops what it is given while the dashboard shows, and forwards it to `inner` otherwise.
    #[must_use]
    pub fn gate(&self, inner: Box<dyn Write>) -> Box<dyn Write> {
        Box::new(Gate {
            visible: Arc::clone(&self.visible),
            inner,
        })
    }

    /// Starts showing. Echo and line buffering stay off until [`Self::hide`] or the drop.
    pub fn show(&self) {
        let mut showing = self.showing.lock().unwrap_or_else(PoisonError::into_inner);
        if showing.is_some() {
            return;
        }
        let quiet = QuietInput::enter(rustix::stdio::stdin()).ok().flatten();
        let stop = Stop::default();
        panics::screen_owned(true);
        self.visible.store(true, Ordering::SeqCst);
        let surface = Arc::clone(&self.surface);
        let state_dir = self.state_dir.clone();
        let since = self.since;
        let visible = Arc::clone(&self.visible);
        let notice = Arc::clone(&self.notice);
        let thread_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("togi-dashboard".to_owned())
            .spawn(move || {
                let drawn = panics::contained(|| surface.draw(&state_dir, since, &thread_stop));
                let failed = match drawn {
                    Ok(Ok(())) => None,
                    Ok(Err(message)) | Err(message) => Some(message),
                };
                if let Some(message) = failed {
                    // The notice comes first: event lines flow again only once it is out.
                    notice(&message);
                    visible.store(false, Ordering::SeqCst);
                    panics::screen_owned(false);
                }
            });
        match thread {
            Ok(thread) => {
                *showing = Some(Showing {
                    stop,
                    thread,
                    quiet,
                });
            }
            Err(error) => {
                self.visible.store(false, Ordering::SeqCst);
                panics::screen_owned(false);
                (self.notice)(&error.to_string());
            }
        }
    }

    /// Whether the dashboard is still on the screen: it has been shown and neither [`Self::hide`] nor a failure
    /// of its surface has ended it.
    #[must_use]
    pub fn is_showing(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
    }

    /// Ends the dashboard, waits for it, and puts the terminal's input back.
    pub fn hide(&self) {
        let showing = self
            .showing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        self.visible.store(false, Ordering::SeqCst);
        if let Some(Showing {
            stop,
            thread,
            quiet,
        }) = showing
        {
            stop.stop();
            let _ = thread.join();
            drop(quiet);
        }
        panics::screen_owned(false);
    }
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        self.hide();
    }
}

struct Gate {
    visible: Arc<AtomicBool>,
    inner: Box<dyn Write>,
}

impl Write for Gate {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.visible.load(Ordering::SeqCst) {
            return Ok(buf.len());
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.visible.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests;
