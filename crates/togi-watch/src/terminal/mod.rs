//! The terminal loop of `togi watch`: it decides when to draw, and the frames decide what.
//!
//! Everything here is synchronous from outside. Each public call drives its own current-thread tokio runtime and
//! returns with no task and no thread still running.
//!
//! # What the screen guarantees
//!
//! - It is cleared once on entry, and again, whole, on a resize or a view change.
//! - Frames are drawn into the screen less its last row and last column, which are never written, so nothing
//!   scrolls. ratatui writes only the cells that changed.
//! - The cursor is hidden while the dashboard shows, and on the Linux console (`TERM=linux`) its 16 colours are
//!   redefined. Both are restored on every exit: quitting, a signal, an error and a panic.
//!
//! # When it redraws
//!
//! On a journal change, a key, a resize, and for the clock as the frame's [`Hold`](crate::screen::Hold) says:
//! every second, at an instant, or never.

mod clock;
mod console;
mod input;
mod keys;
mod notify;
mod palette;
mod show;
mod signals;
mod source;

#[cfg(test)]
mod testing;

use std::io::{self, Write};
use std::os::fd::BorrowedFd;
use std::path::Path;
use std::sync::Arc;

use jiff::Timestamp;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

use crate::screen::{Colours, Drawn, Journal, Screen, View};
use console::Tty;
use input::Reader;
use notify::{Handshake, Watch as _};
use source::Source;

pub use source::SourceError;

/// A failure of the notifications that tell the loop the journal changed.
#[derive(Debug, thiserror::Error)]
#[error("{operation}: {source}")]
pub struct WatchError {
    operation: &'static str,
    #[source]
    source: io::Error,
}

/// Why the dashboard stopped.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Drawing on the terminal failed.
    #[error("{operation}: {source}")]
    Terminal {
        /// What was being done.
        operation: &'static str,
        /// What the system said.
        #[source]
        source: io::Error,
    },
    /// The keyboard could not be set up.
    #[error("read keys: {0}")]
    Keys(#[source] io::Error),
    /// The signal handlers could not be installed.
    #[error("watch signals: {0}")]
    Signals(#[source] io::Error),
    /// The runtime could not be started.
    #[error("start the watch runtime: {0}")]
    Runtime(#[source] io::Error),
    /// The journal notifications failed.
    #[error("watch journal: {0}")]
    Watch(#[from] WatchError),
    /// The journal cannot be shown.
    #[error(transparent)]
    Journal(#[from] SourceError),
    /// The frame could not be written.
    #[error("write frame: {0}")]
    Output(#[source] io::Error),
}

/// What the loop reacts to. One queue carries every source, so reactions happen in the order things happened.
#[derive(Debug)]
enum Event {
    Key(keys::Key),
    /// The keyboard ended.
    InputClosed,
    /// The terminal was resized.
    Resize,
    /// SIGINT, SIGTERM or SIGHUP arrived, or the caller stopped the dashboard.
    Cancel,
    /// The journal changed.
    Changed,
    /// The journal notifications failed.
    Failed(WatchError),
}

/// Lets another thread end a running dashboard.
#[derive(Debug, Clone, Default)]
pub struct Stop(Arc<Notify>);

impl Stop {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ends the dashboard, now or as soon as it starts.
    pub fn stop(&self) {
        self.0.notify_one();
    }
}

/// How to run the live dashboard.
#[derive(Debug, Clone, Copy)]
pub struct Live<'a> {
    /// The state directory holding `events.jsonl`.
    pub state_dir: &'a Path,
    /// For the dashboard of a run that is about to append to the journal: the sequence number of its last event
    /// before the run. Until the journal holds a later one the screen says the run is starting, instead of showing
    /// what an earlier run left in it.
    pub since: Option<u64>,
    /// Ends the dashboard from another thread.
    pub stop: Option<&'a Stop>,
}

/// Aborts its task when dropped.
struct Abort(JoinHandle<()>);

impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Puts a terminal into raw mode, and back when dropped.
struct RawMode<'a> {
    fd: BorrowedFd<'a>,
    saved: rustix::termios::Termios,
}

impl<'a> RawMode<'a> {
    fn enter(fd: BorrowedFd<'a>) -> io::Result<Option<Self>> {
        use rustix::termios::{OptionalActions, isatty, tcgetattr, tcsetattr};
        if !isatty(fd) {
            return Ok(None);
        }
        let saved = tcgetattr(fd)?;
        let mut raw = saved.clone();
        raw.make_raw();
        tcsetattr(fd, OptionalActions::Flush, &raw)?;
        Ok(Some(Self { fd, saved }))
    }
}

impl Drop for RawMode<'_> {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(
            self.fd,
            rustix::termios::OptionalActions::Flush,
            &self.saved,
        );
    }
}

/// Shows the dashboard of the journal on `out` until the viewer quits, SIGINT, SIGTERM or SIGHUP arrives, or
/// `live.stop` is used. When `keys` is a terminal, keys switch views and scroll: `?` explains the screen, `l`
/// shows the event log, the arrow and page keys scroll, `q` quits.
///
/// `render` draws one frame of the journal on a screen into a buffer one row and one column smaller than the
/// terminal, and reports how long the frame stays true.
///
/// SIGWINCH redraws. The handlers of the four signals stay installed after this returns.
pub fn run(
    live: &Live<'_>,
    out: BorrowedFd<'_>,
    keys: Option<BorrowedFd<'_>>,
    render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    let facts = togi_render::OutputFacts::detect(
        &out,
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("JOURNAL_STREAM").as_deref(),
    );
    let colours = palette::colours_for(facts, std::env::var_os("TERM").as_deref());
    let backend = Tty::new(&out).map_err(|source| Error::Terminal {
        operation: "open terminal",
        source,
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Runtime)?;
    let raw = match keys {
        Some(fd) => RawMode::enter(fd).map_err(Error::Keys)?,
        None => None,
    };
    let keys = keys.filter(|_| raw.is_some());
    std::thread::scope(|scope| {
        runtime.block_on(async {
            let (events, mut queue) = mpsc::unbounded_channel();
            let _signals = Abort(signals::listen(events.clone()).map_err(Error::Signals)?);
            let _stop = live.stop.map(|stop| {
                let stop = stop.clone();
                let events = events.clone();
                Abort(tokio::spawn(async move {
                    stop.0.notified().await;
                    let _ = events.send(Event::Cancel);
                }))
            });
            let (reloaded, acknowledged) = mpsc::unbounded_channel();
            let (frozen, frozen_changes) = watch::channel(false);
            let source = std::cell::RefCell::new(Source::new(live.state_dir, live.since));
            let mut watcher = notify::open(live.state_dir)?;
            source.borrow_mut().reload();
            if watcher.polling() {
                watcher.arm()?;
            }
            let mut handshake = Handshake {
                changes: events.clone(),
                reloaded: acknowledged,
                frozen: frozen_changes,
            };
            let _notifier = Abort(tokio::spawn(async move {
                notify::drive(&mut watcher, &mut handshake).await;
            }));
            let _reader = match keys {
                Some(fd) => Some(Reader::start(scope, fd, events.clone()).map_err(Error::Keys)?),
                None => None,
            };
            let mut render = render;
            show::show(
                backend,
                show::Options {
                    palette: colours == Colours::Console,
                    keys: _reader.is_some(),
                    colours,
                    now: Timestamp::now,
                    reloaded: Some(reloaded),
                    frozen: Some(frozen),
                },
                &mut queue,
                |screen, buf| {
                    let mut source = source.borrow_mut();
                    let journal = source.journal(screen);
                    render(&journal, screen, Timestamp::now(), buf)
                },
                || source.borrow_mut().reload(),
            )
            .await
        })
    })
}

/// The width and height of a one-frame print unless told otherwise.
pub const ONE_FRAME_SIZE: (u16, u16) = (240, 67);

/// Prints one frame of `width` by `height` to `out`, without styling. A missing journal prints `no session yet`
/// as the frames say it. A journal that cannot be read or folded prints its problem frame first, and is then
/// returned as the error.
pub fn one_frame(
    state_dir: &Path,
    width: u16,
    height: u16,
    out: &mut impl Write,
    mut render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    let mut source = Source::new(state_dir, None);
    source.reload();
    let screen = Screen {
        view: View::Main,
        keys: false,
        scroll: 0,
        width,
        height,
        one_frame: true,
        colours: Colours::None,
    };
    let mut buffer = Buffer::empty(Rect::new(
        0,
        0,
        width.saturating_sub(1),
        height.saturating_sub(1),
    ));
    render(&source.journal(&screen), &screen, Timestamp::now(), &mut buffer);
    write_plain(&buffer, out).map_err(Error::Output)?;
    source.into_problem().map_or(Ok(()), |error| Err(error.into()))
}

/// Writes the text of a buffer, a line per row, with no styling and without trailing spaces.
fn write_plain(buffer: &Buffer, out: &mut impl Write) -> io::Result<()> {
    let area = buffer.area;
    let mut text = String::new();
    for y in area.top()..area.bottom() {
        let start = text.len();
        let mut skip = 0;
        for x in area.left()..area.right() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            skip = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1);
            text.push_str(symbol);
        }
        text.truncate(text[start..].trim_end().len() + start);
        text.push('\n');
    }
    out.write_all(text.as_bytes())?;
    out.flush()
}
