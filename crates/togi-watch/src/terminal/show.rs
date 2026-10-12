//! The loop: draws a frame, then waits for what makes the next one different.

use jiff::Timestamp;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::{mpsc, watch};

use super::Error;
use super::Event;
use super::clock::Clock;
use super::console::{Console, Session, terminal_error};
use super::keys::press;
use crate::screen::{Colours, Drawn, Hold, Screen, View};

/// What the loop is told about its surroundings.
pub(super) struct Options<N> {
    /// Whether to redefine the console's colours on entry and restore them on exit.
    pub(super) palette: bool,
    /// Whether the reader can press keys.
    pub(super) keys: bool,
    pub(super) colours: Colours,
    /// The wall clock, from which holds are measured.
    pub(super) now: N,
    /// Told after every reload a journal change caused, so the notifier goes on only then.
    pub(super) reloaded: Option<mpsc::UnboundedSender<()>>,
    /// Tells the notifier whether the frame on screen is frozen, so it never polls for one.
    pub(super) frozen: Option<watch::Sender<bool>>,
}

/// Draws into the full screen except its last row and column, which stay as cleared.
struct Painter {
    scratch: Buffer,
}

impl Painter {
    fn paint<B: Console>(
        &mut self,
        session: &mut Session<B>,
        screen: &mut Screen,
        frame: &mut impl FnMut(&Screen, &mut Buffer) -> Drawn,
    ) -> Result<Drawn, Error> {
        session
            .terminal
            .size()
            .map_err(terminal_error("read terminal size"))?;
        session
            .terminal
            .autoresize()
            .map_err(terminal_error("draw frame"))?;
        let full = session.terminal.current_buffer_mut().area;
        screen.width = full.width;
        screen.height = full.height;
        let area = Rect::new(
            0,
            0,
            full.width.saturating_sub(1),
            full.height.saturating_sub(1),
        );
        self.scratch.resize(area);
        self.scratch.reset();
        let drawn = frame(screen, &mut self.scratch);
        let target = session.terminal.current_buffer_mut();
        for y in 0..area.height {
            for x in 0..area.width {
                target[(x, y)] = self.scratch[(x, y)].clone();
            }
        }
        session
            .terminal
            .apply_buffer()
            .map_err(terminal_error("draw frame"))?;
        Ok(drawn)
    }
}

/// Runs the dashboard on `backend` until the viewer quits, `events` says to cancel or ends, or something fails.
///
/// The screen is cleared once on entry and again, whole, on a resize or a view change. It is redrawn on a journal
/// change, a key and a resize, and for the clock as the frame's [`Hold`] says.
pub(super) async fn show<B, N>(
    backend: B,
    options: Options<N>,
    events: &mut mpsc::UnboundedReceiver<Event>,
    mut frame: impl FnMut(&Screen, &mut Buffer) -> Drawn,
    mut reload: impl FnMut() -> bool,
) -> Result<(), Error>
where
    B: Console,
    N: Fn() -> Timestamp,
{
    let mut session = Session::enter(backend, options.palette)?;
    let mut painter = Painter {
        scratch: Buffer::empty(Rect::ZERO),
    };
    let mut clock = Clock::default();
    let mut screen = Screen {
        view: View::Main,
        keys: options.keys,
        scroll: 0,
        width: 0,
        height: 0,
        one_frame: false,
        colours: options.colours,
    };
    let mut repaint = false;
    loop {
        if repaint {
            session.repaint()?;
            repaint = false;
        }
        let drawn = painter.paint(&mut session, &mut screen, &mut frame)?;
        clock.schedule(drawn.hold, (options.now)());
        if let Some(frozen) = &options.frozen {
            frozen.send_if_modified(|frozen| {
                let now = drawn.hold == Hold::Frozen;
                std::mem::replace(frozen, now) != now
            });
        }
        loop {
            tokio::select! {
                biased;
                event = events.recv() => match event {
                    None | Some(Event::Cancel | Event::InputClosed) => return Ok(()),
                    Some(Event::Key(key)) => {
                        let (next, quit) = press(screen, key, drawn.scroll);
                        if quit {
                            return Ok(());
                        }
                        repaint = next.view != screen.view;
                        screen = next;
                        break;
                    }
                    Some(Event::Resize) => {
                        repaint = true;
                        break;
                    }
                    Some(Event::Changed) => {
                        let changed = reload();
                        if let Some(reloaded) = &options.reloaded {
                            let _ = reloaded.send(());
                        }
                        if changed {
                            break;
                        }
                    }
                    Some(Event::Failed(error)) => return Err(Error::Watch(error)),
                },
                () = clock.wake() => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::future::Future;
    use std::panic::AssertUnwindSafe;
    use std::rc::Rc;
    use std::time::Duration;

    use jiff::{SignedDuration, Timestamp};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;
    use tokio::sync::{mpsc, watch};
    use tokio::task::{JoinHandle, LocalSet, spawn_local, yield_now};
    use tokio::time::{Instant, advance, timeout};

    use super::{Options, show};
    use crate::screen::{Colours, Drawn, Hold, Screen, View};
    use crate::terminal::keys::Key;
    use crate::terminal::testing::{Faults, Op, Recorder};
    use crate::terminal::{Error, Event, WatchError};

    const EPOCH: Timestamp = Timestamp::UNIX_EPOCH;

    /// What the frame callback reports and what the reload callback answers, set by the test.
    struct Script {
        screens: RefCell<Vec<Screen>>,
        areas: RefCell<Vec<Rect>>,
        hold: Cell<Hold>,
        scroll: Cell<usize>,
        lines: RefCell<Vec<String>>,
        fill: Cell<bool>,
        reloads: Cell<usize>,
        reloaded_changed: Cell<bool>,
        panic: Cell<bool>,
    }

    struct Harness {
        script: Rc<Script>,
        events: mpsc::UnboundedSender<Event>,
        acknowledged: mpsc::UnboundedReceiver<()>,
        frozen: watch::Receiver<bool>,
        task: JoinHandle<Result<(), Error>>,
        start: Instant,
    }

    struct Setup {
        palette: bool,
        keys: bool,
        colours: Colours,
    }

    impl Default for Setup {
        fn default() -> Self {
            Self {
                palette: false,
                keys: true,
                colours: Colours::None,
            }
        }
    }

    /// Runs a test body on a local set, so the loop runs on the test's own thread under paused time. The bound
    /// is virtual: it only passes when every task is waiting for something that never comes.
    async fn local<F: Future>(body: F) -> F::Output {
        LocalSet::new()
            .run_until(async {
                timeout(Duration::from_hours(1000), body)
                    .await
                    .expect("the test deadlocked")
            })
            .await
    }

    async fn settle() {
        yield_now().await;
        yield_now().await;
    }

    impl Harness {
        fn start(recorder: &Recorder, setup: &Setup) -> Self {
            let script = Rc::new(Script {
                screens: RefCell::default(),
                areas: RefCell::default(),
                hold: Cell::new(Hold::Never),
                scroll: Cell::new(0),
                lines: RefCell::default(),
                fill: Cell::new(false),
                reloads: Cell::new(0),
                reloaded_changed: Cell::new(true),
                panic: Cell::new(false),
            });
            let (events, mut queue) = mpsc::unbounded_channel();
            let (reloaded, acknowledged) = mpsc::unbounded_channel();
            let (frozen, frozen_changes) = watch::channel(false);
            let start = Instant::now();
            let options = Options {
                palette: setup.palette,
                keys: setup.keys,
                colours: setup.colours,
                now: move || EPOCH + SignedDuration::try_from(Instant::now() - start).unwrap(),
                reloaded: Some(reloaded),
                frozen: Some(frozen),
            };
            let backend = recorder.backend();
            let frame_script = script.clone();
            let reload_script = script.clone();
            let task = spawn_local(async move {
                show(
                    backend,
                    options,
                    &mut queue,
                    move |screen, buf: &mut Buffer| frame(&frame_script, screen, buf),
                    move || {
                        reload_script.reloads.set(reload_script.reloads.get() + 1);
                        reload_script.reloaded_changed.get()
                    },
                )
                .await
            });
            Self {
                script,
                events,
                acknowledged,
                frozen: frozen_changes,
                task,
                start,
            }
        }

        fn now(&self) -> Timestamp {
            EPOCH + SignedDuration::try_from(Instant::now() - self.start).unwrap()
        }

        fn frames(&self) -> usize {
            self.script.screens.borrow().len()
        }

        fn last(&self) -> Screen {
            *self.script.screens.borrow().last().unwrap()
        }

        async fn send(&self, event: Event) {
            self.events.send(event).unwrap();
            settle().await;
        }

        async fn key(&self, key: Key) {
            self.send(Event::Key(key)).await;
        }

        async fn finish(self) -> Result<(), Error> {
            self.task.await.unwrap()
        }
    }

    fn frame(script: &Script, screen: &Screen, buf: &mut Buffer) -> Drawn {
        assert!(!script.panic.get(), "frame panic");
        script.screens.borrow_mut().push(*screen);
        script.areas.borrow_mut().push(buf.area);
        if script.fill.get() {
            let area = buf.area;
            for y in 0..area.height {
                for x in 0..area.width {
                    buf[(x, y)].set_symbol("#");
                }
            }
        }
        if !script.fill.get() && buf.area.height > 0 {
            let head = format!("view {:?} scroll {}", screen.view, screen.scroll);
            buf.set_string(0, 0, head, Style::default());
            for (row, line) in script.lines.borrow().iter().enumerate() {
                let row = u16::try_from(row + 1).unwrap();
                if row < buf.area.height {
                    buf.set_string(0, row, line, Style::default());
                }
            }
        }
        Drawn {
            scroll: script.scroll.get(),
            hold: script.hold.get(),
        }
    }

    fn row(cells: &[(u16, u16, String)], y: u16) -> String {
        let mut row: Vec<_> = cells.iter().filter(|cell| cell.1 == y).collect();
        row.sort_by_key(|cell| cell.0);
        row.iter().map(|cell| cell.2.as_str()).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn entry_clears_once_hides_the_cursor_and_draws_all_but_the_last_row_and_column() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            h.script.fill.set(true);
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
            let ops = recorder.ops();
            assert_eq!(ops[0], Op::HideCursor);
            assert_eq!(ops[1], Op::Clear("All".into()));
            assert_eq!(recorder.clears(), 2, "one clear on entry, one on leaving");
            assert!(recorder.restored());
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_first_frame_fills_every_cell_but_the_last_row_and_column_and_nothing_else() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            h.script.fill.set(true);
            h.events.send(Event::Resize).unwrap();
            settle().await;
            let draws = recorder.draws();
            assert_eq!(draws.len(), 2);
            assert_eq!(h.script.areas.borrow()[0], Rect::new(0, 0, 79, 11));
            assert!(draws[0].len() <= 79 * 11);
            for cells in &draws {
                assert!(
                    cells.iter().all(|(x, y, _)| *x < 79 && *y < 11),
                    "a cell of the last row or column was written"
                );
            }
            assert_eq!(
                draws[1].len(),
                79 * 11,
                "a repaint after a clear writes the whole frame"
            );
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn tiny_terminals_draw_nothing_and_do_not_fail() {
        local(async {
            for (width, height) in [(1, 1), (1, 5), (5, 1), (2, 2)] {
                let recorder = Recorder::new(width, height);
                let h = Harness::start(&recorder, &Setup::default());
                h.script.fill.set(true);
                h.send(Event::Resize).await;
                for cells in recorder.draws() {
                    assert!(
                        cells
                            .iter()
                            .all(|(x, y, _)| *x < width - 1 && *y < height - 1)
                    );
                }
                assert_eq!(
                    h.script.areas.borrow()[0],
                    Rect::new(0, 0, width - 1, height - 1)
                );
                h.send(Event::Cancel).await;
                h.finish().await.unwrap();
            }
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn only_changed_rows_are_written() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            *h.script.lines.borrow_mut() = vec!["first".into(), "second".into(), "third".into()];
            h.events.send(Event::Resize).unwrap();
            settle().await;
            let before = recorder.draws().len();
            *h.script.lines.borrow_mut() = vec!["first".into(), "short".into(), "third".into()];
            advance(Duration::from_secs(1)).await;
            settle().await;
            let draws = recorder.draws();
            assert_eq!(draws.len(), before + 1);
            let changed = draws.last().unwrap();
            assert!(changed.iter().all(|cell| cell.1 == 2), "{changed:?}");
            assert_eq!(row(changed, 2), "hort ", "the stale last letter stayed");
            *h.script.lines.borrow_mut() = vec!["first".into()];
            advance(Duration::from_secs(1)).await;
            settle().await;
            let mut shorter = recorder.draws().last().unwrap().clone();
            shorter.sort();
            let blanks: Vec<_> = (0..5)
                .flat_map(|x| (2..4).map(move |y| (x, y, " ".to_owned())))
                .collect();
            assert_eq!(shorter, blanks, "rows the frame left out were not blanked");
            advance(Duration::from_secs(1)).await;
            settle().await;
            assert!(
                recorder.draws().last().unwrap().is_empty(),
                "an unchanged frame wrote cells"
            );
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_resize_clears_and_repaints_the_whole_screen_once() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            h.script.fill.set(true);
            settle().await;
            assert_eq!(recorder.clears(), 1, "startup cleared the screen once");
            recorder.resize(100, 20);
            h.send(Event::Resize).await;
            assert_eq!(
                recorder.clears(),
                2,
                "a resize cleared the screen once more"
            );
            assert_eq!((h.last().width, h.last().height), (100, 20));
            assert_eq!(recorder.draws().last().unwrap().len(), 99 * 19);
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_size_change_without_a_signal_is_still_repainted_whole_and_once() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            h.script.fill.set(true);
            settle().await;
            recorder.resize(100, 20);
            advance(Duration::from_secs(1)).await;
            settle().await;
            assert_eq!(recorder.clears(), 2);
            assert_eq!(recorder.draws().last().unwrap().len(), 99 * 19);
            advance(Duration::from_secs(1)).await;
            settle().await;
            assert_eq!(recorder.clears(), 2, "the next tick did not clear again");
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_clock_redraws_once_a_second_and_a_frozen_frame_never() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            settle().await;
            assert_eq!(h.frames(), 1);
            advance(Duration::from_millis(999)).await;
            settle().await;
            assert_eq!(h.frames(), 1);
            advance(Duration::from_millis(1)).await;
            settle().await;
            assert_eq!(h.frames(), 2);
            h.script.hold.set(Hold::Frozen);
            advance(Duration::from_secs(1)).await;
            settle().await;
            assert_eq!(h.frames(), 3, "the frame that reported the hold");
            let draws = recorder.draws().len();
            advance(Duration::from_hours(6)).await;
            settle().await;
            assert_eq!(h.frames(), 3, "hours of virtual time redrew a frozen frame");
            assert_eq!(recorder.draws().len(), draws);
            assert!(*h.frozen.borrow());
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn evidence_releases_a_frozen_frame_and_the_clock_runs_again() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            h.script.hold.set(Hold::Frozen);
            h.send(Event::Resize).await;
            let frames = h.frames();
            advance(Duration::from_hours(6)).await;
            assert_eq!(h.frames(), frames);
            h.script.hold.set(Hold::Never);
            h.send(Event::Changed).await;
            assert_eq!(h.frames(), frames + 1);
            assert!(!*h.frozen.borrow());
            advance(Duration::from_secs(1)).await;
            settle().await;
            assert_eq!(h.frames(), frames + 2);
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_frame_holds_until_a_journal_change_key_resize_or_its_deadline() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            settle().await;
            let check = |frames: usize, reloads: usize| {
                assert_eq!((h.frames(), h.script.reloads.get()), (frames, reloads));
            };
            check(1, 0);
            advance(Duration::from_secs(1)).await;
            settle().await;
            check(2, 0);
            h.script
                .hold
                .set(Hold::Until(h.now() + SignedDuration::from_secs(20)));
            h.send(Event::Changed).await;
            check(3, 1);
            advance(Duration::from_secs(5)).await;
            settle().await;
            check(3, 1);
            h.send(Event::Changed).await;
            check(4, 2);
            h.key(Key::Char('?')).await;
            check(5, 2);
            h.send(Event::Resize).await;
            check(6, 2);
            advance(Duration::from_secs(14)).await;
            settle().await;
            check(6, 2);
            advance(Duration::from_secs(1)).await;
            settle().await;
            check(7, 2);
            advance(Duration::from_secs(1)).await;
            settle().await;
            check(8, 2);
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_change_that_shows_nothing_new_redraws_nothing_but_is_acknowledged() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let mut h = Harness::start(&recorder, &Setup::default());
            settle().await;
            h.script.reloaded_changed.set(false);
            h.send(Event::Changed).await;
            assert_eq!((h.frames(), h.script.reloads.get()), (1, 1));
            assert!(h.acknowledged.try_recv().is_ok());
            h.script.reloaded_changed.set(true);
            h.send(Event::Changed).await;
            assert_eq!(h.frames(), 2);
            assert!(h.acknowledged.try_recv().is_ok());
            assert!(h.acknowledged.try_recv().is_err());
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn keys_switch_views_and_scroll_and_a_view_change_repaints_whole() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            h.script.scroll.set(20);
            settle().await;
            assert_eq!(recorder.clears(), 1);
            let follow = usize::MAX;
            let steps = [
                (Key::Char('?'), View::Help, 0, true),
                (Key::Down, View::Help, 1, false),
                (Key::PageDown, View::Help, 5, false),
                (Key::Home, View::Help, 0, false),
                (Key::End, View::Help, 20, false),
                (Key::Char('?'), View::Main, 0, true),
                (Key::Down, View::Main, 0, false),
                (Key::Char('L'), View::Log, follow, true),
                (Key::Up, View::Log, 19, false),
                (Key::Down, View::Log, follow, false),
                (Key::Up, View::Log, 19, false),
            ];
            for (key, view, scroll, repainted) in steps {
                let clears = recorder.clears();
                h.key(key).await;
                let screen = h.last();
                assert_eq!((screen.view, screen.scroll), (view, scroll), "{key:?}");
                assert!(screen.keys);
                assert_eq!(
                    recorder.clears() - clears,
                    usize::from(repainted),
                    "{key:?}"
                );
            }
            h.script.scroll.set(25);
            advance(Duration::from_secs(1)).await;
            settle().await;
            assert_eq!((h.last().view, h.last().scroll), (View::Log, 19));
            h.key(Key::End).await;
            assert_eq!(h.last().scroll, follow);
            h.script.scroll.set(30);
            advance(Duration::from_secs(1)).await;
            settle().await;
            assert_eq!(
                h.last().scroll,
                follow,
                "a followed log stays at the end as it grows"
            );
            for (key, view) in [
                (Key::Esc, View::Main),
                (Key::Char('h'), View::Help),
                (Key::Char('l'), View::Log),
                (Key::Char('l'), View::Main),
            ] {
                let clears = recorder.clears();
                h.key(key).await;
                assert_eq!(h.last().view, view);
                assert_eq!(recorder.clears(), clears + 1, "{key:?}");
            }
            let frames = h.frames();
            h.key(Key::Char('q')).await;
            assert_eq!(h.frames(), frames, "quitting redrew");
            h.finish().await.unwrap();
            assert!(recorder.restored());
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_screen_tells_the_frame_its_size_keys_and_colours() {
        local(async {
            for colours in [Colours::Full, Colours::Console, Colours::None] {
                let recorder = Recorder::new(90, 30);
                let h = Harness::start(
                    &recorder,
                    &Setup {
                        palette: false,
                        keys: false,
                        colours,
                    },
                );
                settle().await;
                let screen = h.last();
                assert_eq!(
                    (
                        screen.width,
                        screen.height,
                        screen.keys,
                        screen.one_frame,
                        screen.colours
                    ),
                    (90, 30, false, false, colours)
                );
                h.send(Event::Cancel).await;
                h.finish().await.unwrap();
            }
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_console_palette_is_set_on_entry_and_reset_on_every_exit() {
        local(async {
            for palette in [true, false] {
                for exit in ["cancel", "input closed", "quit", "failure"] {
                    let recorder = Recorder::new(80, 12);
                    let h = Harness::start(
                        &recorder,
                        &Setup {
                            palette,
                            ..Setup::default()
                        },
                    );
                    settle().await;
                    let result = match exit {
                        "cancel" => Event::Cancel,
                        "input closed" => Event::InputClosed,
                        "quit" => Event::Key(Key::Char('q')),
                        _ => Event::Failed(WatchError {
                            operation: "watch journal",
                            source: std::io::Error::other("gone"),
                        }),
                    };
                    h.send(result).await;
                    let outcome = h.finish().await;
                    assert_eq!(outcome.is_err(), exit == "failure");
                    let raw = recorder.raw();
                    if palette {
                        assert_eq!(raw.first().unwrap(), &super::super::console::set_palette());
                        assert_eq!(raw.last().unwrap(), super::super::console::RESET_PALETTE);
                    } else {
                        assert!(!raw.iter().any(|text| text.contains("\x1b]")), "{raw:?}");
                    }
                    assert!(recorder.restored(), "{exit}");
                }
            }
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn the_loop_ends_when_nothing_can_send_to_it_any_more() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            settle().await;
            let Harness { events, task, .. } = h;
            drop(events);
            task.await.unwrap().unwrap();
            assert!(recorder.restored());
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_notification_ends_the_loop_with_its_error_and_restores_the_terminal() {
        local(async {
            let recorder = Recorder::new(80, 12);
            let h = Harness::start(&recorder, &Setup::default());
            h.send(Event::Failed(WatchError {
                operation: "read journal notifications",
                source: std::io::Error::other("overflow"),
            }))
            .await;
            let error = h.finish().await.unwrap_err();
            assert_eq!(
                error.to_string(),
                "watch journal: read journal notifications: overflow"
            );
            assert!(recorder.restored());
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_failures_name_the_operation_keep_the_cause_and_restore_what_was_set_up() {
        local(async {
            let cases = [
                (
                    "clear terminal",
                    Faults {
                        clear: true,
                        ..Faults::default()
                    },
                    0,
                ),
                (
                    "read terminal size",
                    Faults {
                        size_from: Some(3),
                        ..Faults::default()
                    },
                    0,
                ),
                (
                    "draw frame",
                    Faults {
                        draw_from: Some(1),
                        ..Faults::default()
                    },
                    0,
                ),
                (
                    "read terminal size",
                    Faults {
                        size_from: Some(5),
                        ..Faults::default()
                    },
                    1,
                ),
                (
                    "draw frame",
                    Faults {
                        draw_from: Some(2),
                        ..Faults::default()
                    },
                    1,
                ),
            ];
            for (operation, faults, good_frames) in cases {
                let recorder = Recorder::failing(80, 12, faults);
                let h = Harness::start(
                    &recorder,
                    &Setup {
                        palette: true,
                        ..Setup::default()
                    },
                );
                settle().await;
                assert_eq!(
                    recorder.draws().len(),
                    good_frames,
                    "{operation}: the first frame"
                );
                if good_frames > 0 {
                    advance(Duration::from_secs(1)).await;
                }
                let error = h.finish().await.unwrap_err();
                let Error::Terminal {
                    operation: got,
                    source,
                } = &error
                else {
                    panic!("{error:?}");
                };
                assert_eq!(*got, operation);
                assert_eq!(source.to_string(), "terminal unavailable");
                let raw = recorder.raw();
                assert_eq!(
                    raw.last().map(String::as_str),
                    Some(super::super::console::RESET_PALETTE),
                    "{operation}: the palette was not restored"
                );
            }
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_redraw_size_read_is_reported_after_a_good_first_frame() {
        local(async {
            let recorder = Recorder::failing(
                80,
                12,
                Faults {
                    size_from: Some(5),
                    ..Faults::default()
                },
            );
            let h = Harness::start(&recorder, &Setup::default());
            settle().await;
            assert_eq!(h.frames(), 1);
            h.events.send(Event::Resize).unwrap();
            let error = h.finish().await.unwrap_err();
            assert!(
                matches!(
                    error,
                    Error::Terminal {
                        operation: "read terminal size",
                        ..
                    }
                ),
                "{error:?}"
            );
            assert!(recorder.restored());
        })
        .await;
    }

    #[test]
    fn a_panic_in_the_frame_restores_the_terminal_before_it_propagates() {
        let recorder = Recorder::new(80, 12);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (events, mut queue) = mpsc::unbounded_channel();
        let backend = recorder.backend();
        let caught = std::panic::catch_unwind(AssertUnwindSafe(|| {
            runtime.block_on(show(
                backend,
                Options {
                    palette: true,
                    keys: false,
                    colours: Colours::Console,
                    now: Timestamp::now,
                    reloaded: None,
                    frozen: None,
                },
                &mut queue,
                |_: &Screen, _: &mut Buffer| -> Drawn { panic!("session panic") },
                || false,
            ))
        }));
        drop(events);
        let payload = caught.unwrap_err();
        assert_eq!(payload.downcast_ref::<&str>(), Some(&"session panic"));
        assert!(recorder.restored());
        assert_eq!(
            recorder.raw().last().map(String::as_str),
            Some(super::super::console::RESET_PALETTE)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_shrinking_terminal_is_cleared_and_repainted_whole() {
        local(async {
            let recorder = Recorder::new(100, 20);
            let h = Harness::start(&recorder, &Setup::default());
            h.script.fill.set(true);
            settle().await;
            let clears = recorder.clears();
            recorder.resize(60, 10);
            h.send(Event::Resize).await;
            assert!(recorder.clears() > clears);
            assert_eq!(recorder.draws().last().unwrap().len(), 59 * 9);
            let clears = recorder.clears();
            advance(Duration::from_secs(1)).await;
            settle().await;
            assert_eq!(
                recorder.clears(),
                clears,
                "the tick after the resize cleared again"
            );
            h.send(Event::Cancel).await;
            h.finish().await.unwrap();
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_live_journal_problem_stays_on_screen_and_recovers_when_the_journal_does() {
        use crate::terminal::source::Source;
        use crate::terminal::testing::{session_with_warnings, write_bytes, write_journal};
        use std::cell::RefCell;

        local(async {
            let dir = tempfile::TempDir::new().unwrap();
            let source = Rc::new(RefCell::new(Source::new(dir.path(), None)));
            source.borrow_mut().reload();
            let recorder = Recorder::new(160, 40);
            let (events, mut queue) = mpsc::unbounded_channel();
            let shown: Rc<RefCell<Vec<String>>> = Rc::default();
            let frame_source = source.clone();
            let frame_shown = shown.clone();
            let reload_source = source.clone();
            let options = Options {
                palette: false,
                keys: false,
                colours: Colours::None,
                now: || EPOCH,
                reloaded: None,
                frozen: None,
            };
            let backend = recorder.backend();
            let task = spawn_local(async move {
                show(
                    backend,
                    options,
                    &mut queue,
                    move |screen, _: &mut Buffer| {
                        let mut source = frame_source.borrow_mut();
                        let text = match source.journal(screen) {
                            crate::screen::Journal::Missing => "no session yet".to_owned(),
                            crate::screen::Journal::Starting => "starting".to_owned(),
                            crate::screen::Journal::Problem(error) => error.to_string(),
                            crate::screen::Journal::Ready { log, .. } => {
                                format!("{} events", log.total)
                            }
                        };
                        frame_shown.borrow_mut().push(text);
                        Drawn {
                            scroll: 0,
                            hold: Hold::Frozen,
                        }
                    },
                    move || reload_source.borrow_mut().reload(),
                )
                .await
            });
            settle().await;
            write_bytes(dir.path(), b"not json\n");
            events.send(Event::Changed).unwrap();
            settle().await;
            write_journal(dir.path(), &session_with_warnings(2));
            events.send(Event::Changed).unwrap();
            settle().await;
            events.send(Event::Cancel).unwrap();
            task.await.unwrap().unwrap();
            let shown = shown.borrow();
            assert_eq!(shown.len(), 3, "one frame per journal change: {shown:?}");
            assert_eq!(shown[0], "no session yet");
            assert!(shown[1].contains("line 1"), "{shown:?}");
            assert_eq!(shown[2], "3 events");
        })
        .await;
    }
}
