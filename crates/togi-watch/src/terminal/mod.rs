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
    let raw = match keys {
        Some(fd) => RawMode::enter(fd).map_err(Error::Keys)?,
        None => None,
    };
    drive(
        backend,
        live,
        keys.filter(|_| raw.is_some()),
        colours,
        Timestamp::now,
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
    live: &Live<'_>,
    keys: Option<BorrowedFd<'_>>,
    colours: Colours,
    now: impl Fn() -> Timestamp,
    mut render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
) -> Result<(), Error> {
    let now = &now;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Runtime)?;
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
            let reader = match keys {
                Some(fd) => Some(Reader::start(scope, fd, events.clone()).map_err(Error::Keys)?),
                None => None,
            };
            show::show(
                backend,
                show::Options {
                    palette: uses_palette(colours),
                    keys: reader.is_some(),
                    colours,
                    now,
                    reloaded: Some(reloaded),
                    frozen: Some(frozen),
                },
                &mut queue,
                |screen, buf| {
                    let mut source = source.borrow_mut();
                    let journal = source.journal(screen);
                    render(&journal, screen, now(), buf)
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
    use std::os::fd::{AsFd as _, OwnedFd};
    use std::panic::AssertUnwindSafe;

    use jiff::Timestamp;
    use ratatui::buffer::Buffer;
    use rustix::process::{Signal, getpid, kill_process};
    use tempfile::TempDir;

    use super::testing::{Faults, Recorder, session_with_warnings, write_journal};
    use super::{
        Colours, Drawn, Error, Live, ONE_FRAME_SIZE, SourceError, Stop, View, drive, one_frame, palette,
        uses_palette,
    };
    use crate::screen::{Hold, Journal, Screen};

    fn pipe() -> (OwnedFd, OwnedFd) {
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap()
    }

    fn plain(_: &Journal<'_>, _: &Screen, _: Timestamp, buf: &mut Buffer) -> Drawn {
        buf.set_string(0, 0, "frame", ratatui::style::Style::default());
        Drawn {
            scroll: 0,
            hold: Hold::Never,
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
        live: &Live<'_>,
        keys: Option<&OwnedFd>,
        colours: Colours,
        render: impl FnMut(&Journal<'_>, &Screen, Timestamp, &mut Buffer) -> Drawn,
    ) -> Result<(), Error> {
        drive(
            recorder.backend(),
            live,
            keys.map(AsFd::as_fd),
            colours,
            Timestamp::now,
            render,
        )
    }

    use std::os::fd::AsFd;

    #[test]
    fn the_reader_is_joined_on_every_exit() {
        let state = TempDir::new().unwrap();
        for exit in ["quit", "end of input", "stop", "clear error", "size error", "draw error", "panic"] {
            let (read, write) = pipe();
            let stop = Stop::new();
            let live = Live {
                state_dir: state.path(),
                since: None,
                stop: Some(&stop),
            };
            let faults = match exit {
                "clear error" => Faults { clear: true, ..Faults::default() },
                "size error" => Faults { size_from: Some(3), ..Faults::default() },
                "draw error" => Faults { draw_from: Some(1), ..Faults::default() },
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
                run_drive(&recorder, &live, Some(&read), Colours::None, |journal, screen, now, buf| {
                    assert!(exit != "panic", "frame panic");
                    plain(journal, screen, now, buf)
                })
            }));
            match exit {
                "panic" => assert!(result.is_err()),
                "clear error" | "size error" | "draw error" => {
                    assert!(matches!(result.unwrap(), Err(Error::Terminal { .. })), "{exit}");
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
    fn each_of_the_three_signals_ends_the_dashboard_and_restores_the_terminal() {
        let state = TempDir::new().unwrap();
        for (name, signal) in [("SIGINT", Signal::INT), ("SIGTERM", Signal::TERM), ("SIGHUP", Signal::HUP)] {
            let recorder = Recorder::new(80, 12);
            let live = Live {
                state_dir: state.path(),
                since: None,
                stop: None,
            };
            let mut sent = false;
            run_drive(&recorder, &live, None, Colours::Console, |journal, screen, now, buf| {
                if !sent {
                    sent = true;
                    kill_process(getpid(), signal).unwrap();
                }
                plain(journal, screen, now, buf)
            })
            .unwrap();
            assert!(recorder.restored(), "{name}");
            assert_eq!(recorder.raw().last().map(String::as_str), Some(palette::RESET), "{name}: the palette");
        }
    }

    #[test]
    fn sigwinch_redraws_with_the_new_size() {
        let state = TempDir::new().unwrap();
        let recorder = Recorder::new(80, 12);
        let live = Live {
            state_dir: state.path(),
            since: None,
            stop: None,
        };
        let mut sizes = Vec::new();
        let probe = recorder.clone();
        run_drive(&recorder, &live, None, Colours::None, |journal, screen, now, buf| {
            sizes.push((screen.width, screen.height));
            match sizes.len() {
                1 => {
                    probe.resize(100, 30);
                    kill_process(getpid(), Signal::WINCH).unwrap();
                }
                _ => kill_process(getpid(), Signal::TERM).unwrap(),
            }
            plain(journal, screen, now, buf)
        })
        .unwrap();
        assert_eq!(sizes, [(80, 12), (100, 30)]);
        assert_eq!(recorder.clears(), 3, "entry, the resize, and leaving");
    }

    #[test]
    fn stopping_before_the_dashboard_starts_ends_it_at_once() {
        let state = TempDir::new().unwrap();
        let stop = Stop::new();
        stop.stop();
        let recorder = Recorder::new(80, 12);
        let live = Live {
            state_dir: state.path(),
            since: None,
            stop: Some(&stop),
        };
        run_drive(&recorder, &live, None, Colours::None, plain).unwrap();
        assert!(recorder.restored());
    }

    #[test]
    fn another_thread_can_stop_a_running_dashboard() {
        let state = TempDir::new().unwrap();
        let stop = Stop::new();
        let recorder = Recorder::new(80, 12);
        let live = Live {
            state_dir: state.path(),
            since: None,
            stop: Some(&stop),
        };
        let other = stop.clone();
        let mut frames = 0;
        run_drive(&recorder, &live, None, Colours::None, |journal, screen, now, buf| {
            frames += 1;
            std::thread::scope(|scope| {
                scope.spawn(|| other.stop());
            });
            plain(journal, screen, now, buf)
        })
        .unwrap();
        assert_eq!(frames, 1);
        assert!(recorder.restored());
    }

    #[test]
    fn the_console_palette_is_redefined_only_for_the_console_profile() {
        let state = TempDir::new().unwrap();
        for colours in [Colours::Full, Colours::Console, Colours::None] {
            let recorder = Recorder::new(80, 12);
            let stop = Stop::new();
            stop.stop();
            let live = Live {
                state_dir: state.path(),
                since: None,
                stop: Some(&stop),
            };
            run_drive(&recorder, &live, None, colours, plain).unwrap();
            let raw = recorder.raw();
            let palette = raw.iter().any(|text| text.starts_with("\x1b]P"));
            assert_eq!(palette, colours == Colours::Console, "{colours:?}");
            assert_eq!(uses_palette(colours), colours == Colours::Console);
            assert_eq!(
                raw.iter().any(|text| text == super::palette::RESET),
                colours == Colours::Console,
                "{colours:?}: restored"
            );
        }
    }

    #[test]
    fn the_frame_is_drawn_from_the_journal_on_disk() {
        let state = TempDir::new().unwrap();
        write_journal(state.path(), &session_with_warnings(3));
        let recorder = Recorder::new(80, 12);
        let stop = Stop::new();
        stop.stop();
        let live = Live {
            state_dir: state.path(),
            since: None,
            stop: Some(&stop),
        };
        let mut seen = Vec::new();
        run_drive(&recorder, &live, None, Colours::None, |journal, screen, now, buf| {
            seen.push(match journal {
                Journal::Ready { log, .. } => log.total,
                _ => 0,
            });
            plain(journal, screen, now, buf)
        })
        .unwrap();
        assert_eq!(seen, [4]);
    }

    /// A stand-in for the frames: says what the journal is, in the words the frames use for a missing one.
    fn describe(journal: &Journal<'_>, screen: &Screen, _: Timestamp, buf: &mut Buffer) -> Drawn {
        let words = match journal {
            Journal::Missing => "no session yet".to_owned(),
            Journal::Starting => "starting".to_owned(),
            Journal::Problem(error) => format!("problem: {error}"),
            Journal::Ready { log, .. } => format!("{} events", log.total),
        };
        let style = ratatui::style::Style::default().fg(ratatui::style::Color::Red).bold();
        buf.set_string(0, 0, words, style);
        buf.set_string(0, 1, "日本  ", style);
        buf.set_string(0, 2, format!("{}x{} one_frame={} keys={}", screen.width, screen.height, screen.one_frame, screen.keys), style);
        Drawn {
            scroll: 0,
            hold: Hold::Never,
        }
    }

    #[test]
    fn one_frame_of_a_missing_journal_prints_the_frame_without_styling_and_succeeds() {
        let state = TempDir::new().unwrap();
        let mut out = Vec::new();
        one_frame(state.path(), 40, 6, &mut out, describe).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text, "no session yet\n日本\n40x6 one_frame=true keys=false\n\n\n");
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn one_frame_draws_into_the_screen_less_its_last_row_and_column() {
        let state = TempDir::new().unwrap();
        let mut out = Vec::new();
        let mut area = None;
        let mut screen_seen = None;
        one_frame(state.path(), 30, 9, &mut out, |journal, screen, now, buf| {
            area = Some(buf.area);
            screen_seen = Some(*screen);
            describe(journal, screen, now, buf)
        })
        .unwrap();
        assert_eq!(area, Some(ratatui::layout::Rect::new(0, 0, 29, 8)));
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 8);
        let screen = screen_seen.unwrap();
        assert_eq!((screen.colours, screen.view, screen.scroll), (Colours::None, View::Main, 0));
    }

    #[test]
    fn one_frame_defaults_to_240_by_67() {
        assert_eq!(ONE_FRAME_SIZE, (240, 67));
    }

    #[test]
    fn one_frame_of_a_journal_shows_its_session() {
        let state = TempDir::new().unwrap();
        write_journal(state.path(), &session_with_warnings(2));
        let mut out = Vec::new();
        one_frame(state.path(), 40, 6, &mut out, describe).unwrap();
        assert!(String::from_utf8(out).unwrap().starts_with("3 events\n"));
    }

    #[test]
    fn one_frame_of_an_unreadable_journal_prints_its_problem_frame_then_returns_the_error() {
        let state = TempDir::new().unwrap();
        super::testing::write_bytes(state.path(), b"not json\n");
        let mut out = Vec::new();
        let error = one_frame(state.path(), 80, 6, &mut out, describe).unwrap_err();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("problem: "), "{text}");
        assert!(text.contains("line 1"), "{text}");
        assert!(matches!(&error, Error::Journal(SourceError::Read(_))), "{error:?}");
        assert!(error.to_string().contains("line 1"));
    }

    #[test]
    fn one_frame_of_a_journal_that_cannot_fold_returns_that_error_too() {
        let state = TempDir::new().unwrap();
        write_journal(state.path(), &[super::testing::session_start(1), super::testing::session_start(2)]);
        let mut out = Vec::new();
        let error = one_frame(state.path(), 80, 6, &mut out, describe).unwrap_err();
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
        let state = TempDir::new().unwrap();
        let error = one_frame(state.path(), 10, 6, &mut Broken, describe).unwrap_err();
        assert!(matches!(error, Error::Output(_)));
    }
}
