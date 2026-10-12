//! The loop: draws a frame, then waits for what makes the next one different.

use jiff::Timestamp;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::{mpsc, watch};

use super::Event;
use super::Error;
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
