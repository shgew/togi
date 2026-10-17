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
//! - SIGWINCH redraws, and for [`run`] SIGINT, SIGTERM and SIGHUP end the dashboard. The listeners are tokio's,
//!   which stay registered for the rest of the process: once a dashboard has run, none of these signals takes its
//!   default action, so a caller keeps its own listener for SIGINT, SIGTERM and SIGHUP.
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
#[cfg(test)]
#[cfg(target_os = "linux")]
mod real_process;
mod show;
mod signals;
mod source;

#[cfg(test)]
mod testing;

use std::io::{self, Write};
use std::os::fd::BorrowedFd;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

use crate::frames;
use crate::screen::{Colours, Drawn, Journal, Quit, Screen, View};
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
    /// The signals could not be listened for.
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

/// How the dashboard is run: what drives its reloads and redraws, and what ends it.
#[derive(Debug, Clone, Copy)]
pub struct Config<'a> {
    /// The state directory whose changes make the loop ask its [`Feed`] to reload, or `None` when the feed changes
    /// by itself and is only asked to draw.
    pub watch: Option<&'a Path>,
    /// A redraw at least this often, whatever the frame's [`Hold`](crate::screen::Hold) says, for a feed that
    /// changes with time.
    pub tick: Option<Duration>,
    /// Another thread's way to end the dashboard.
    pub stop: Option<&'a Stop>,
    /// Whether SIGINT, SIGTERM and SIGHUP end the dashboard.
    pub terminating: bool,
    /// What `q`, `Q`, Ctrl-C and Ctrl-D do.
    pub quit: Quit,
}

/// The journal a dashboard shows, which the caller owns.
pub trait Feed {
    /// The journal for a frame of `screen`.
    fn journal(&mut self, screen: &Screen) -> Journal<'_>;
    /// Reloads after the watched directory changed, and reports whether what is shown changed. It is also called
    /// once before the first frame.
    fn reload(&mut self) -> bool;
    /// Takes the error that ends the dashboard rather than being shown, after a reload. A feed whose journal can
    /// always be shown has none.
    fn refusal(&mut self) -> Option<SourceError> {
        None
    }
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

/// Shows the dashboard of the journal in `state_dir` on `out` until the viewer quits or SIGINT, SIGTERM or
/// SIGHUP arrives. When `keys` is a terminal, keys change page and scroll: the left and right arrows cycle the nine
/// pages, `?` opens the help, `l` the journal and Esc or `m` the main page, the up and down, page, Home and End keys
/// scroll, and `q`, `Q`, Ctrl-C and Ctrl-D quit unless `quit` is [`Quit::Ignored`], where they do nothing on any
/// page and the hints leave them out. Frames are drawn by
/// [`frames::render`] in the system's time zone, in the colour profile [`frames::colours_for`] chooses for `out`,
/// `NO_COLOR`, `JOURNAL_STREAM` and `TERM`.
///
/// It listens for SIGINT, SIGTERM, SIGHUP and SIGWINCH (which redraws) through tokio, whose listeners stay
/// registered for the rest of the process: once it has run, these signals no longer take their default action, so
/// the caller keeps its own listener for SIGINT, SIGTERM and SIGHUP for the process's lifetime.
pub fn run(
    state_dir: &Path,
    out: BorrowedFd<'_>,
    keys: Option<BorrowedFd<'_>>,
    quit: Quit,
) -> Result<(), Error> {
    run_rendered(state_dir, out, keys, quit, dashboard())
}

/// [`run`] on a journal and a clock the caller owns, which stays the only loop there is: the frames draw `feed`'s
/// journal at the time `now` gives, in the system's time zone, and `config` says what drives the redraws and what
/// ends the dashboard. The terminal, the keys and the signals behave as for [`run`], with SIGINT, SIGTERM and
/// SIGHUP ending it only when `config.terminating` is set.
pub fn run_with(
    feed: impl Feed,
    config: &Config<'_>,
    now: impl Fn() -> Timestamp,
    out: BorrowedFd<'_>,
    keys: Option<BorrowedFd<'_>>,
) -> Result<(), Error> {
    start(feed, config, out, keys, now, dashboard())
}

/// [`run`] with `render` drawing each frame of the journal on a screen into a buffer one row and one column smaller
/// than the terminal, and reporting how long the frame stays true.
fn run_rendered(
    state_dir: &Path,
    out: BorrowedFd<'_>,
    keys: Option<BorrowedFd<'_>>,
    quit: Quit,
    render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    let mut source = Source::new(state_dir, None);
    refuse_foreign(&mut source)?;
    start(
        source,
        &Config {
            watch: Some(state_dir),
            tick: None,
            stop: None,
            terminating: true,
            quit,
        },
        out,
        keys,
        Timestamp::now,
        render,
    )
}

/// Loads the journal for a dashboard that has not touched the terminal yet, and refuses another build's journal
/// before it does.
fn refuse_foreign(source: &mut Source) -> Result<(), Error> {
    source.reload();
    source
        .refusal()
        .map_or(Ok(()), |refusal| Err(refusal.into()))
}

/// [`run`] for a journal a run is about to append to, which takes its cancellation from the caller. Until the
/// journal holds an event after sequence number `since`, the last one before the run, the screen says the run is
/// starting instead of showing what an earlier run left in it. It ends when the viewer quits or `stop` is used,
/// and leaves SIGINT, SIGTERM and SIGHUP to the caller: it listens only for SIGWINCH.
pub fn run_after(
    state_dir: &Path,
    since: u64,
    stop: &Stop,
    out: BorrowedFd<'_>,
    keys: Option<BorrowedFd<'_>>,
) -> Result<(), Error> {
    run_after_rendered(state_dir, since, stop, out, keys, dashboard())
}

/// [`run_after`] with `render` drawing each frame, as for [`run_rendered`].
fn run_after_rendered(
    state_dir: &Path,
    since: u64,
    stop: &Stop,
    out: BorrowedFd<'_>,
    keys: Option<BorrowedFd<'_>>,
    render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    let mut source = Source::new(state_dir, Some(since));
    refuse_foreign(&mut source)?;
    start(
        source,
        &Config {
            watch: Some(state_dir),
            tick: None,
            stop: Some(stop),
            terminating: false,
            quit: Quit::Keys,
        },
        out,
        keys,
        Timestamp::now,
        render,
    )
}

/// The dashboard's frames: [`frames::render`] in the system's time zone.
fn dashboard() -> impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn {
    let zone = TimeZone::system();
    move |journal, screen, now, buf| frames::render(journal, screen, now, &zone, buf)
}

fn start(
    feed: impl Feed,
    config: &Config<'_>,
    out: BorrowedFd<'_>,
    keys: Option<BorrowedFd<'_>>,
    now: impl Fn() -> Timestamp,
    render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    let facts = togi_render::OutputFacts::detect(
        &out,
        std::env::var_os("NO_COLOR").as_deref(),
        std::env::var_os("JOURNAL_STREAM").as_deref(),
    );
    let colours = frames::colours_for(facts, std::env::var_os("TERM").as_deref());
    let backend = Tty::new(&out).map_err(|source| Error::Terminal {
        operation: "open terminal",
        source,
    })?;
    let raw = match keys {
        Some(fd) => RawMode::enter(fd).map_err(Error::Keys)?,
        None => None,
    };
    drive(
        backend,
        feed,
        config,
        keys.filter(|_| raw.is_some()),
        colours,
        now,
        render,
    )
}

/// Whether the console's palette is redefined for a colour profile: only the Linux console's own is.
fn uses_palette(colours: Colours) -> bool {
    colours == Colours::Console
}

/// [`run`] on any backend, with the terminal's raw mode and colour profile already decided by the caller.
fn drive<B: console::Console>(
    backend: B,
    feed: impl Feed,
    live: &Config<'_>,
    keys: Option<BorrowedFd<'_>>,
    colours: Colours,
    now: impl Fn() -> Timestamp,
    render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Runtime)?;
    drive_with(
        &runtime,
        backend,
        feed,
        live,
        keys,
        colours,
        now,
        render,
        signals::listen,
    )
}

/// The driver with its runtime and event listener supplied; the production adapter owns both.
#[expect(
    clippy::too_many_arguments,
    reason = "The driver preserves its inputs while injecting runtime and signal setup."
)]
fn drive_with<B: console::Console>(
    runtime: &tokio::runtime::Runtime,
    backend: B,
    feed: impl Feed,
    live: &Config<'_>,
    keys: Option<BorrowedFd<'_>>,
    colours: Colours,
    now: impl Fn() -> Timestamp,
    mut render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
    listen: impl FnOnce(bool, mpsc::UnboundedSender<Event>) -> io::Result<JoinHandle<()>>,
) -> Result<(), Error> {
    let now = &now;
    std::thread::scope(|scope| {
        runtime.block_on(async {
            let (events, mut queue) = mpsc::unbounded_channel();
            let _signals = Abort(listen(live.terminating, events.clone()).map_err(Error::Signals)?);
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
            let feed = std::cell::RefCell::new(feed);
            let refused = std::cell::RefCell::new(None);
            let mut watcher = live.watch.map(notify::open).transpose()?;
            feed.borrow_mut().reload();
            if let Some(refusal) = feed.borrow_mut().refusal() {
                return Err(refusal.into());
            }
            let _notifier = match watcher.take() {
                Some(mut watcher) => {
                    if watcher.polling() {
                        watcher.arm()?;
                    }
                    let mut handshake = Handshake {
                        changes: events.clone(),
                        reloaded: acknowledged,
                        frozen: frozen_changes,
                    };
                    Some(Abort(tokio::spawn(async move {
                        notify::drive(&mut watcher, &mut handshake).await;
                    })))
                }
                None => None,
            };
            let reader = match keys {
                Some(fd) => Some(Reader::start(scope, fd, events.clone()).map_err(Error::Keys)?),
                None => None,
            };
            let shown = show::show(
                backend,
                show::Options {
                    palette: uses_palette(colours),
                    keys: reader.is_some(),
                    quit: live.quit,
                    colours,
                    now,
                    tick: live.tick,
                    reloaded: Some(reloaded),
                    frozen: Some(frozen),
                },
                &mut queue,
                |screen, buf| {
                    let mut feed = feed.borrow_mut();
                    let journal = feed.journal(screen);
                    render(&journal, screen, now(), buf)
                },
                || {
                    let mut feed = feed.borrow_mut();
                    let changed = feed.reload();
                    match feed.refusal() {
                        Some(refusal) => {
                            *refused.borrow_mut() = Some(refusal);
                            let _ = events.send(Event::Cancel);
                            false
                        }
                        None => changed,
                    }
                },
            )
            .await;
            match refused.take() {
                Some(refusal) => Err(refusal.into()),
                None => shown,
            }
        })
    })
}

/// The width and height of a one-frame print unless told otherwise.
pub const ONE_FRAME_SIZE: (u16, u16) = (240, 67);

/// Prints one frame of `width` by `height` of the journal in `state_dir` to `out`, drawn by [`frames::render`] in
/// the system's time zone, without styling. A missing journal prints `NO SESSION YET` as the frames say it. A
/// journal that cannot be read or folded prints its problem frame first, and is then returned as the error.
pub fn one_frame(
    state_dir: &Path,
    width: u16,
    height: u16,
    out: &mut impl Write,
) -> Result<(), Error> {
    one_frame_with(state_dir, width, height, out, dashboard())
}

/// [`one_frame`] using the caller's filesystem, instant and time zone instead of the host's.
///
/// The journal is read through `fs` once; the same refusal and problem-frame rules apply.
pub fn one_frame_in(
    fs: &dyn togi_journal::fs::Fs,
    state_dir: &Path,
    width: u16,
    height: u16,
    out: &mut impl Write,
    now: Timestamp,
    zone: &TimeZone,
) -> Result<(), Error> {
    one_frame_loaded(
        Source::read_in(fs, state_dir, None),
        width,
        height,
        out,
        now,
        |journal, screen, now, buffer| frames::render(journal, screen, now, zone, buffer),
    )
}

/// [`one_frame`] with `render` drawing the frame, as for [`run_rendered`].
fn one_frame_with(
    state_dir: &Path,
    width: u16,
    height: u16,
    out: &mut impl Write,
    render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    one_frame_source(
        Source::new(state_dir, None),
        width,
        height,
        out,
        Timestamp::now(),
        render,
    )
}

/// Reloads a supplied journal and draws it at a supplied instant.
fn one_frame_source(
    mut source: Source,
    width: u16,
    height: u16,
    out: &mut impl Write,
    now: Timestamp,
    render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    source.reload();
    one_frame_loaded(source, width, height, out, now, render)
}

/// Draws an already-loaded journal without consulting the host.
fn one_frame_loaded(
    mut source: Source,
    width: u16,
    height: u16,
    out: &mut impl Write,
    now: Timestamp,
    mut render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    if let Some(refusal) = source.refusal() {
        return Err(refusal.into());
    }
    let screen = Screen {
        view: View::Main,
        keys: false,
        scroll: 0,
        width,
        height,
        one_frame: true,
        colours: Colours::None,
        quit: Quit::Keys,
    };
    let mut buffer = Buffer::empty(Rect::new(
        0,
        0,
        width.saturating_sub(1),
        height.saturating_sub(1),
    ));
    render(&source.journal(&screen), &screen, now, &mut buffer);
    write_plain(&buffer, out).map_err(Error::Output)?;
    source
        .into_problem()
        .map_or(Ok(()), |error| Err(error.into()))
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
            skip = ratatui::text::Span::raw(symbol).width().saturating_sub(1);
            text.push_str(symbol);
        }
        text.truncate(text[start..].trim_end().len() + start);
        text.push('\n');
    }
    out.write_all(text.as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::os::fd::OwnedFd;
    use std::panic::AssertUnwindSafe;

    use jiff::Timestamp;
    use ratatui::buffer::Buffer;

    use super::console::RESET_PALETTE;
    use super::keys::Key;
    use super::source::MemoryJournal;
    use super::testing::{Faults, Recorder, lines, session_with_warnings};
    use super::{
        Colours, Config, Drawn, Error, Event, Feed, ONE_FRAME_SIZE, Source, SourceError, Stop,
        View, drive_with, one_frame_source, uses_palette,
    };
    use crate::screen::{Hold, Journal, Quit, Screen};

    fn pipe() -> (OwnedFd, OwnedFd) {
        super::input::pipe().unwrap()
    }

    fn plain(_: &Journal<'_>, _: &Screen, _: Timestamp, buf: &mut Buffer) -> Drawn {
        buf.set_string(0, 0, "frame", ratatui::style::Style::default());
        Drawn {
            scroll: 0,
            hold: Hold::Never,
        }
    }

    /// [`plain`], frozen: only a signal, a key or a stop draws again.
    fn frozen(journal: &Journal<'_>, screen: &Screen, now: Timestamp, buf: &mut Buffer) -> Drawn {
        Drawn {
            hold: Hold::Frozen,
            ..plain(journal, screen, now, buf)
        }
    }

    /// The reader is gone and has taken nothing: what is written to the pipe now is all still in it.
    fn assert_pipe_untouched(read: &OwnedFd, write: &OwnedFd) {
        rustix::io::write(write, b"remaining input").unwrap();
        let mut buf = [0_u8; 15];
        let mut filled = 0;
        while filled < buf.len() {
            filled += rustix::io::read(read, &mut buf[filled..]).unwrap();
        }
        assert_eq!(&buf, b"remaining input");
    }

    fn run_drive(
        recorder: &Recorder,
        live: &Config<'_>,
        feed: impl Feed,
        colours: Colours,
        render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
    ) -> Result<(), Error> {
        drive_with(
            &timer_runtime(),
            recorder.backend(),
            feed,
            live,
            None,
            colours,
            || Timestamp::UNIX_EPOCH,
            render,
            silent_listener,
        )
    }

    fn timer_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap()
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the injected listener has a fallible start interface"
    )]
    fn silent_listener(
        _: bool,
        _: tokio::sync::mpsc::UnboundedSender<Event>,
    ) -> std::io::Result<tokio::task::JoinHandle<()>> {
        Ok(tokio::spawn(std::future::pending()))
    }

    fn run_events(
        recorder: &Recorder,
        live: &Config<'_>,
        feed: impl Feed,
        colours: Colours,
        mut render: impl FnMut(
            &Journal<'_>,
            &Screen,
            Timestamp,
            &mut Buffer,
            &tokio::sync::mpsc::UnboundedSender<Event>,
        ) -> Drawn,
    ) -> Result<(), Error> {
        let events = std::cell::RefCell::new(None);
        drive_with(
            &timer_runtime(),
            recorder.backend(),
            feed,
            live,
            None,
            colours,
            || Timestamp::UNIX_EPOCH,
            |journal, screen, now, buf| {
                render(journal, screen, now, buf, events.borrow().as_ref().unwrap())
            },
            |terminating, sender| {
                *events.borrow_mut() = Some(sender.clone());
                silent_listener(terminating, sender)
            },
        )
    }

    fn one_frame_with(
        journal: &MemoryJournal,
        width: u16,
        height: u16,
        out: &mut impl std::io::Write,
        render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
    ) -> Result<(), Error> {
        one_frame_source(
            Source::memory(journal, None),
            width,
            height,
            out,
            Timestamp::UNIX_EPOCH,
            render,
        )
    }

    fn one_frame(
        journal: &MemoryJournal,
        width: u16,
        height: u16,
        out: &mut impl std::io::Write,
    ) -> Result<(), Error> {
        one_frame_with(
            journal,
            width,
            height,
            out,
            |journal, screen, now, buffer| {
                crate::frames::render(journal, screen, now, &jiff::tz::TimeZone::UTC, buffer)
            },
        )
    }

    use std::os::fd::AsFd;

    struct Counting {
        reloads: std::rc::Rc<std::cell::Cell<u32>>,
        journals: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl Feed for Counting {
        fn journal(&mut self, _: &Screen) -> Journal<'_> {
            self.journals.set(self.journals.get() + 1);
            Journal::Missing
        }

        fn reload(&mut self) -> bool {
            self.reloads.set(self.reloads.get() + 1);
            false
        }
    }

    #[test]
    fn a_caller_owned_feed_and_clock_drive_the_one_loop_with_no_directory_watched() {
        let reloads = std::rc::Rc::new(std::cell::Cell::new(0));
        let journals = std::rc::Rc::new(std::cell::Cell::new(0));
        let feed = Counting {
            reloads: reloads.clone(),
            journals: journals.clone(),
        };
        let stop = Stop::new();
        let config = Config {
            watch: None,
            tick: Some(std::time::Duration::from_millis(10)),
            stop: Some(&stop),
            terminating: false,
            quit: Quit::Keys,
        };
        let reads = std::cell::Cell::new(0_i64);
        let clock = || {
            reads.set(reads.get() + 1);
            Timestamp::from_second(reads.get()).unwrap()
        };
        let watchdog = Watchdog::start({
            let stop = stop.clone();
            move || stop.stop()
        });
        let mut drawn_at = Vec::new();
        let recorder = Recorder::new(80, 12);
        drive_with(
            &timer_runtime(),
            recorder.backend(),
            feed,
            &config,
            None,
            Colours::None,
            clock,
            |journal, screen, now, buf| {
                assert!(matches!(journal, Journal::Missing));
                drawn_at.push(now);
                if drawn_at.len() == 3 {
                    stop.stop();
                }
                frozen(journal, screen, now, buf)
            },
            silent_listener,
        )
        .unwrap();
        assert!(!watchdog.fired(), "a frozen frame was never redrawn");
        assert_eq!(reloads.get(), 1, "one reload before the first frame");
        assert_eq!(journals.get(), 3, "the feed was asked for each frame");
        assert!(
            drawn_at.windows(2).all(|pair| pair[0] < pair[1]),
            "frames are drawn at the caller's clock: {drawn_at:?}"
        );
        assert!(recorder.restored());
    }

    #[test]
    fn real_process_the_reader_is_joined_on_every_exit() {
        let state = MemoryJournal::new();
        for exit in [
            "quit",
            "end of input",
            "stop",
            "clear error",
            "size error",
            "draw error",
            "panic",
        ] {
            let (read, write) = pipe();
            let stop = Stop::new();
            let live = Config {
                watch: None,
                tick: None,
                stop: Some(&stop),
                terminating: false,
                quit: Quit::Keys,
            };
            let faults = match exit {
                "clear error" => Faults {
                    clear: true,
                    ..Faults::default()
                },
                "size error" => Faults {
                    size_from: Some(3),
                    ..Faults::default()
                },
                "draw error" => Faults {
                    draw_from: Some(1),
                    ..Faults::default()
                },
                _ => Faults::default(),
            };
            let recorder = Recorder::failing(80, 12, faults);
            match exit {
                "quit" => {
                    rustix::io::write(&write, b"q").unwrap();
                }
                "stop" => stop.stop(),
                _ => {}
            }
            let write = if exit == "end of input" {
                drop(write);
                None
            } else {
                Some(write)
            };
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                drive_with(
                    &tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap(),
                    recorder.backend(),
                    Source::memory(&state, None),
                    &live,
                    Some(read.as_fd()),
                    Colours::None,
                    || Timestamp::UNIX_EPOCH,
                    |journal, screen, now, buf| {
                        assert!(exit != "panic", "frame panic");
                        plain(journal, screen, now, buf)
                    },
                    silent_listener,
                )
            }));
            match exit {
                "panic" => assert!(result.is_err()),
                "clear error" | "size error" | "draw error" => {
                    assert!(
                        matches!(result.unwrap(), Err(Error::Terminal { .. })),
                        "{exit}"
                    );
                }
                _ => result.unwrap().unwrap(),
            }
            assert!(recorder.restored(), "{exit}");
            if let Some(write) = &write {
                assert_pipe_untouched(&read, write);
            }
        }
    }

    #[test]
    fn cancellation_ends_the_dashboard_and_restores_the_terminal() {
        let state = MemoryJournal::new();
        for name in ["SIGINT", "SIGTERM", "SIGHUP"] {
            let recorder = Recorder::new(80, 12);
            let stop = Stop::new();
            let live = Config {
                watch: None,
                tick: None,
                stop: Some(&stop),
                terminating: true,
                quit: Quit::Keys,
            };
            let watchdog = Watchdog::start({
                let stop = stop.clone();
                move || stop.stop()
            });
            let mut sent = false;
            run_events(
                &recorder,
                &live,
                Source::memory(&state, None),
                Colours::Console,
                |journal, screen, now, buf, events| {
                    if !sent {
                        sent = true;
                        events.send(Event::Cancel).unwrap();
                    }
                    frozen(journal, screen, now, buf)
                },
            )
            .unwrap();
            assert!(!watchdog.fired(), "{name} did not end the dashboard");
            assert!(recorder.restored(), "{name}");
            assert_eq!(
                recorder.raw().last().map(String::as_str),
                Some(RESET_PALETTE),
                "{name}: the palette"
            );
        }
    }

    /// What a journal that cannot show a session is.
    #[derive(Debug, Clone, Copy)]
    enum Without {
        Missing,
        Starting,
        Problem,
    }

    /// A feed of one journal state that holds no session.
    struct Bare {
        without: Without,
        error: io::Error,
    }

    impl Feed for Bare {
        fn journal(&mut self, _: &Screen) -> Journal<'_> {
            match self.without {
                Without::Missing => Journal::Missing,
                Without::Starting => Journal::Starting,
                Without::Problem => Journal::Problem(&self.error),
            }
        }

        fn reload(&mut self) -> bool {
            false
        }
    }

    #[test]
    fn the_quit_policy_holds_on_every_page_of_a_missing_starting_or_unreadable_journal() {
        for without in [Without::Missing, Without::Starting, Without::Problem] {
            for quit in [Quit::Keys, Quit::Ignored] {
                for (at, page) in View::ALL.into_iter().enumerate() {
                    let recorder = Recorder::new(80, 12);
                    let stop = Stop::new();
                    let live = Config {
                        watch: None,
                        tick: None,
                        stop: Some(&stop),
                        terminating: false,
                        quit,
                    };
                    let watchdog = Watchdog::start({
                        let stop = stop.clone();
                        move || stop.stop()
                    });
                    let mut sent = false;
                    let mut pages = Vec::new();
                    run_events(
                        &recorder,
                        &live,
                        Bare {
                            without,
                            error: io::Error::other("unreadable"),
                        },
                        Colours::None,
                        |journal, screen, now, buf, events| {
                            assert_eq!(
                                matches!(journal, Journal::Missing),
                                matches!(without, Without::Missing)
                            );
                            pages.push(screen.view);
                            if !sent {
                                sent = true;
                                for _ in 0..at {
                                    events.send(Event::Key(Key::Right)).unwrap();
                                }
                                for quit_key in ['q', 'Q', '\x03', '\x04'] {
                                    events.send(Event::Key(Key::Char(quit_key))).unwrap();
                                }
                                events.send(Event::Cancel).unwrap();
                            }
                            plain(journal, screen, now, buf)
                        },
                    )
                    .unwrap();
                    assert!(!watchdog.fired(), "{without:?} {quit:?} {page:?} hung");
                    let want = match quit {
                        Quit::Keys => at + 1,
                        Quit::Ignored => at + 5,
                    };
                    assert_eq!(
                        pages.len(),
                        want,
                        "{without:?} {quit:?} {page:?}: a quit key {} the dashboard",
                        if quit == Quit::Keys { "left" } else { "ended" }
                    );
                    assert_eq!(pages.last(), Some(&page), "{without:?} {quit:?}");
                }
            }
        }
    }

    /// How long a test waits for a dashboard that should already have ended.
    const BOUND: std::time::Duration = std::time::Duration::from_secs(20);

    /// Runs `fire` if it is not finished within [`BOUND`], so that a dashboard waiting for what never comes ends
    /// and its test fails instead of hanging.
    struct Watchdog {
        done: Option<std::sync::mpsc::Sender<()>>,
        thread: Option<std::thread::JoinHandle<bool>>,
    }

    impl Watchdog {
        fn start(fire: impl FnOnce() + Send + 'static) -> Self {
            let (done, finished) = std::sync::mpsc::channel::<()>();
            let thread = std::thread::spawn(move || {
                let fired =
                    finished.recv_timeout(BOUND) == Err(std::sync::mpsc::RecvTimeoutError::Timeout);
                if fired {
                    fire();
                }
                fired
            });
            Self {
                done: Some(done),
                thread: Some(thread),
            }
        }

        /// Whether it had to fire.
        fn fired(mut self) -> bool {
            self.done.take();
            self.thread.take().unwrap().join().unwrap()
        }
    }

    impl Drop for Watchdog {
        fn drop(&mut self) {
            self.done.take();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[test]
    fn a_resize_redraws_with_the_new_size() {
        let state = MemoryJournal::new();
        let recorder = Recorder::new(80, 12);
        let stop = Stop::new();
        let live = Config {
            watch: None,
            tick: None,
            stop: Some(&stop),
            terminating: true,
            quit: Quit::Keys,
        };
        let watchdog = Watchdog::start({
            let stop = stop.clone();
            move || stop.stop()
        });
        let mut sizes = Vec::new();
        let probe = recorder.clone();
        run_events(
            &recorder,
            &live,
            Source::memory(&state, None),
            Colours::None,
            |journal, screen, now, buf, events| {
                sizes.push((screen.width, screen.height));
                match sizes.len() {
                    1 => {
                        probe.resize(100, 30);
                        events.send(Event::Resize).unwrap();
                    }
                    _ => events.send(Event::Cancel).unwrap(),
                }
                frozen(journal, screen, now, buf)
            },
        )
        .unwrap();
        assert!(!watchdog.fired(), "the resize drew nothing");
        assert_eq!(sizes, [(80, 12), (100, 30)]);
        assert_eq!(recorder.clears(), 3, "entry, the resize, and leaving");
    }

    #[test]
    fn stopping_before_the_dashboard_starts_ends_it_at_once() {
        let state = MemoryJournal::new();
        let stop = Stop::new();
        stop.stop();
        let recorder = Recorder::new(80, 12);
        let live = Config {
            watch: None,
            tick: None,
            stop: Some(&stop),
            terminating: false,
            quit: Quit::Keys,
        };
        run_drive(
            &recorder,
            &live,
            Source::memory(&state, None),
            Colours::None,
            plain,
        )
        .unwrap();
        assert!(recorder.restored());
    }

    #[test]
    fn another_thread_can_stop_a_running_dashboard() {
        let state = MemoryJournal::new();
        let stop = Stop::new();
        let recorder = Recorder::new(80, 12);
        let live = Config {
            watch: None,
            tick: None,
            stop: Some(&stop),
            terminating: true,
            quit: Quit::Keys,
        };
        let watchdog = Watchdog::start({
            let stop = stop.clone();
            move || stop.stop()
        });
        let other = stop.clone();
        let mut frames = 0;
        run_drive(
            &recorder,
            &live,
            Source::memory(&state, None),
            Colours::None,
            |journal, screen, now, buf| {
                frames += 1;
                std::thread::scope(|scope| {
                    scope.spawn(|| other.stop());
                });
                frozen(journal, screen, now, buf)
            },
        )
        .unwrap();
        assert!(!watchdog.fired(), "the stop from another thread was lost");
        assert_eq!(frames, 1);
        assert!(recorder.restored());
    }

    #[test]
    fn the_console_palette_is_redefined_only_for_the_console_profile() {
        let state = MemoryJournal::new();
        for colours in [Colours::Full, Colours::Console, Colours::None] {
            let recorder = Recorder::new(80, 12);
            let stop = Stop::new();
            stop.stop();
            let live = Config {
                watch: None,
                tick: None,
                stop: Some(&stop),
                terminating: false,
                quit: Quit::Keys,
            };
            run_drive(
                &recorder,
                &live,
                Source::memory(&state, None),
                colours,
                plain,
            )
            .unwrap();
            let raw = recorder.raw();
            let palette = raw.iter().any(|text| text.starts_with("\x1b]P"));
            assert_eq!(palette, colours == Colours::Console, "{colours:?}");
            assert_eq!(uses_palette(colours), colours == Colours::Console);
            assert_eq!(
                raw.iter().any(|text| text == RESET_PALETTE),
                colours == Colours::Console,
                "{colours:?}: restored"
            );
        }
    }

    #[test]
    fn the_frame_is_drawn_from_the_journal() {
        let state = MemoryJournal::new();
        state.write(&lines(&session_with_warnings(3)));
        let recorder = Recorder::new(80, 12);
        let stop = Stop::new();
        stop.stop();
        let live = Config {
            watch: None,
            tick: None,
            stop: Some(&stop),
            terminating: false,
            quit: Quit::Keys,
        };
        let mut seen = Vec::new();
        run_drive(
            &recorder,
            &live,
            Source::memory(&state, None),
            Colours::None,
            |journal, screen, now, buf| {
                seen.push(match journal {
                    Journal::Ready { log, .. } => log.total,
                    _ => 0,
                });
                plain(journal, screen, now, buf)
            },
        )
        .unwrap();
        assert_eq!(seen, [4]);
    }

    /// A stand-in for the frames: says what the journal is, in the words the frames use for a missing one.
    fn describe(journal: &Journal<'_>, screen: &Screen, _: Timestamp, buf: &mut Buffer) -> Drawn {
        let words = match journal {
            Journal::Missing => "no session yet".to_owned(),
            Journal::Starting => "starting".to_owned(),
            Journal::Problem(error) => format!("problem: {}", togi_render::error_chain(error)),
            Journal::Ready { log, .. } => format!("{} events", log.total),
        };
        let style = ratatui::style::Style::default()
            .fg(ratatui::style::Color::Red)
            .bold();
        buf.set_string(0, 0, words, style);
        buf.set_string(0, 1, "日本  ", style);
        buf.set_string(
            0,
            2,
            format!(
                "{}x{} one_frame={} keys={}",
                screen.width, screen.height, screen.one_frame, screen.keys
            ),
            style,
        );
        Drawn {
            scroll: 0,
            hold: Hold::Never,
        }
    }

    #[test]
    fn one_frame_of_a_missing_journal_prints_the_frame_without_styling_and_succeeds() {
        let state = MemoryJournal::new();
        let mut out = Vec::new();
        one_frame_with(&state, 40, 6, &mut out, describe).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(
            text,
            "no session yet\n日本\n40x6 one_frame=true keys=false\n\n\n"
        );
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn one_frame_draws_into_the_screen_less_its_last_row_and_column() {
        let state = MemoryJournal::new();
        let mut out = Vec::new();
        let mut area = None;
        let mut screen_seen = None;
        one_frame_with(&state, 30, 9, &mut out, |journal, screen, now, buf| {
            area = Some(buf.area);
            screen_seen = Some(*screen);
            describe(journal, screen, now, buf)
        })
        .unwrap();
        assert_eq!(area, Some(ratatui::layout::Rect::new(0, 0, 29, 8)));
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 8);
        let screen = screen_seen.unwrap();
        assert_eq!(
            (screen.colours, screen.view, screen.scroll),
            (Colours::None, View::Main, 0)
        );
    }

    #[test]
    fn one_frame_defaults_to_240_by_67() {
        assert_eq!(ONE_FRAME_SIZE, (240, 67));
    }

    #[test]
    fn one_frame_of_a_journal_shows_its_session() {
        let state = MemoryJournal::new();
        state.write(&lines(&session_with_warnings(2)));
        let mut out = Vec::new();
        one_frame_with(&state, 40, 6, &mut out, describe).unwrap();
        assert!(String::from_utf8(out).unwrap().starts_with("3 events\n"));
    }

    #[test]
    fn one_frame_of_an_unreadable_journal_prints_its_problem_frame_then_returns_the_error() {
        let state = MemoryJournal::new();
        state.write(&super::testing::undecodable_journal());
        let mut out = Vec::new();
        let error = one_frame_with(&state, 400, 6, &mut out, describe).unwrap_err();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("problem: "), "{text}");
        assert!(text.contains("line 2"), "{text}");
        assert!(
            matches!(&error, Error::Journal(SourceError::Read(_))),
            "{error:?}"
        );
        assert!(togi_render::error_chain(&error).contains("line 2"));
    }

    #[test]
    fn one_frame_of_a_journal_that_cannot_fold_returns_that_error_too() {
        let state = MemoryJournal::new();
        state.write(&lines(&[
            super::testing::session_start(1),
            super::testing::session_start(2),
        ]));
        let mut out = Vec::new();
        let error = one_frame_with(&state, 80, 6, &mut out, describe).unwrap_err();
        assert!(matches!(error, Error::Journal(SourceError::Fold(_))));
        assert!(String::from_utf8(out).unwrap().starts_with("problem: "));
    }

    #[test]
    fn one_frame_reports_a_failed_write() {
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let state = MemoryJournal::new();
        let error = one_frame_with(&state, 10, 6, &mut Broken, describe).unwrap_err();
        assert!(matches!(error, Error::Output(_)));
    }

    #[test]
    fn one_frame_prints_the_dashboard_frame_without_styling() {
        let state = MemoryJournal::new();
        let mut out = Vec::new();
        one_frame(&state, 120, 33, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("NO SESSION YET"), "{text}");
        assert_eq!(text.lines().count(), 32);
        assert!(!text.contains('\x1b'), "{text}");
        state.write(&lines(&togi_watchtest::Scenario::Idle.events().unwrap()));
        let mut out = Vec::new();
        one_frame(&state, 120, 33, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("screen paused"), "{text}");
        assert!(
            text.lines().all(|line| line.chars().count() <= 119),
            "{text}"
        );
    }

    #[test]
    fn one_frame_prints_the_dashboard_s_problem_frame_and_returns_the_error() {
        let state = MemoryJournal::new();
        state.write(&super::testing::undecodable_journal());
        let mut out = Vec::new();
        let error = one_frame(&state, 120, 33, &mut out).unwrap_err();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("JOURNAL UNREADABLE"), "{text}");
        assert!(!text.contains('\x1b'), "{text}");
        assert!(
            matches!(error, Error::Journal(SourceError::Read(_))),
            "{error:?}"
        );
    }

    #[test]
    fn one_frame_of_another_builds_journal_prints_nothing_and_leaves_the_file_alone() {
        let state = MemoryJournal::new();
        state.write(&super::testing::go_journal());
        let before = state.snapshot();
        for width in [120, 400] {
            let mut out = Vec::new();
            let error = one_frame(&state, width, 33, &mut out).unwrap_err();
            assert!(out.is_empty(), "a refusal drew a frame");
            assert!(
                matches!(error, Error::Journal(SourceError::Foreign(_))),
                "{error:?}"
            );
            let text = error.to_string();
            assert!(text.contains("events.jsonl"), "{text}");
            assert!(text.contains("docs/howto.md#go-to-rust-cutover"), "{text}");
        }
        let after = state.snapshot();
        assert_eq!(before.0, after.0);
        assert_eq!(before.1, after.1);
        assert_eq!(before.3, after.3, "a refusal mutated the journal");
    }

    #[test]
    fn a_dashboard_refuses_another_builds_journal_before_it_draws_anything() {
        let state = MemoryJournal::new();
        state.write(&super::testing::go_journal());
        let recorder = Recorder::new(80, 12);
        let stop = Stop::new();
        let live = Config {
            watch: None,
            tick: None,
            stop: Some(&stop),
            terminating: false,
            quit: Quit::Keys,
        };
        let error = run_drive(
            &recorder,
            &live,
            Source::memory(&state, None),
            Colours::None,
            plain,
        )
        .unwrap_err();
        assert!(
            matches!(error, Error::Journal(SourceError::Foreign(_))),
            "{error:?}"
        );
        assert!(recorder.ops().is_empty(), "{:?}", recorder.ops());
    }

    #[test]
    fn a_live_dashboard_ends_with_the_refusal_when_another_builds_journal_arrives() {
        let state = MemoryJournal::new();
        let recorder = Recorder::new(80, 12);
        let stop = Stop::new();
        let live = Config {
            watch: None,
            tick: None,
            stop: Some(&stop),
            terminating: false,
            quit: Quit::Keys,
        };
        let watchdog = Watchdog::start({
            let stop = stop.clone();
            move || stop.stop()
        });
        let result = run_events(
            &recorder,
            &live,
            Source::memory(&state, None),
            Colours::None,
            |journal, screen, now, buf, events| {
                state.write(&super::testing::go_journal());
                events.send(Event::Changed).unwrap();
                plain(journal, screen, now, buf)
            },
        );
        assert!(!watchdog.fired(), "the changed journal was never reloaded");
        let error = result.unwrap_err();
        assert!(
            matches!(error, Error::Journal(SourceError::Foreign(_))),
            "{error:?}"
        );
        assert!(
            recorder.restored(),
            "the terminal was left as the dashboard set it"
        );
    }
}
