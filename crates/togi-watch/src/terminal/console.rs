//! The terminal as the loop draws on it: its backend, and the guard that restores it on every exit.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::os::fd::AsFd;

use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Rect, Size};

use super::palette;
use super::Error;

/// A ratatui backend that also takes raw bytes, for the sequences ratatui has no call for.
pub(super) trait Console: Backend<Error = io::Error> + Write {}

impl<T: Backend<Error = io::Error> + Write> Console for T {}

/// The real terminal: crossterm's backend writing to a descriptor, whose size comes from that descriptor and
/// not from whichever stream is standard output.
pub(super) struct Tty {
    backend: CrosstermBackend<BufWriter<File>>,
    out: File,
}

impl Tty {
    pub(super) fn new(out: &impl AsFd) -> io::Result<Self> {
        let out = File::from(out.as_fd().try_clone_to_owned()?);
        let writer = BufWriter::new(out.try_clone()?);
        Ok(Self {
            backend: CrosstermBackend::new(writer),
            out,
        })
    }

    fn winsize(&self) -> io::Result<rustix::termios::Winsize> {
        Ok(rustix::termios::tcgetwinsize(&self.out)?)
    }
}

impl Write for Tty {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.backend.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.backend)
    }
}

impl Backend for Tty {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.backend.draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.backend.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.backend.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.backend.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.backend.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.backend.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        let size = self.winsize()?;
        Ok(Size::new(size.ws_col, size.ws_row))
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        let size = self.winsize()?;
        Ok(WindowSize {
            columns_rows: Size::new(size.ws_col, size.ws_row),
            pixels: Size::new(size.ws_xpixel, size.ws_ypixel),
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.backend)
    }
}

pub(super) fn terminal_error(operation: &'static str) -> impl Fn(io::Error) -> Error {
    move |source| Error::Terminal { operation, source }
}

/// The terminal while the dashboard owns it. Entering clears the screen, hides the cursor and, on the Linux
/// console, redefines its colours. Dropping restores all of it, on a normal exit, an error and a panic alike.
pub(super) struct Session<B: Console> {
    pub(super) terminal: Terminal<B>,
    palette: bool,
}

impl<B: Console> Session<B> {
    pub(super) fn enter(backend: B, palette: bool) -> Result<Self, Error> {
        let terminal = Terminal::new(backend).map_err(terminal_error("read terminal size"))?;
        let mut session = Self { terminal, palette };
        session.clear().map_err(terminal_error("clear terminal"))?;
        Ok(session)
    }

    fn clear(&mut self) -> io::Result<()> {
        let backend = self.terminal.backend_mut();
        if self.palette {
            backend.write_all(palette::set().as_bytes())?;
        }
        backend.hide_cursor()?;
        let size = self.terminal.size()?;
        self.terminal.resize(Rect::from(size))?;
        Backend::flush(self.terminal.backend_mut())
    }

    /// Repaints the whole screen on the next frame, after clearing it.
    pub(super) fn repaint(&mut self) -> Result<(), Error> {
        let size = self
            .terminal
            .size()
            .map_err(terminal_error("read terminal size"))?;
        self.terminal
            .resize(Rect::from(size))
            .map_err(terminal_error("clear terminal"))
    }
}

impl<B: Console> Drop for Session<B> {
    fn drop(&mut self) {
        let backend = self.terminal.backend_mut();
        let _ = backend.write_all(b"\x1b[0m");
        let _ = backend.clear_region(ClearType::All);
        let _ = backend.set_cursor_position(Position::ORIGIN);
        let _ = self.terminal.show_cursor();
        let backend = self.terminal.backend_mut();
        if self.palette {
            let _ = backend.write_all(palette::RESET.as_bytes());
        }
        let _ = Backend::flush(backend);
    }
}
