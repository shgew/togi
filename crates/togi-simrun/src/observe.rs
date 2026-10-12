use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use jiff::tz::TimeZone;
use togi_journal::{Event, MemFs};
use togi_machine::CancellationToken;
use togi_render::Renderer;

use crate::input::{Observe, Observer};

pub(crate) type SharedLog = Rc<RefCell<UserLog>>;
pub(crate) type Slot = Rc<RefCell<Option<Observer>>>;

const EVENTS: &str = "/state/events.jsonl";

/// The caller's log. The first write that fails drops it and is kept for the simulation to report, so the
/// observer behind the same [`Tee`] keeps seeing every appended event.
pub(crate) struct UserLog {
    writer: Option<Box<dyn Write>>,
    failure: Option<io::Error>,
}

impl UserLog {
    pub(crate) fn new(writer: Option<Box<dyn Write>>) -> Self {
        Self {
            writer,
            failure: None,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.attempt(|writer| writer.write_all(bytes));
    }

    pub(crate) fn write_event(&mut self, renderer: Renderer, event: &Event, zone: &TimeZone) {
        self.attempt(|writer| renderer.write_event(writer, event, zone));
    }

    fn attempt(&mut self, write: impl FnOnce(&mut dyn Write) -> io::Result<()>) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        if let Err(error) = write(writer.as_mut()) {
            self.writer = None;
            self.failure = Some(error);
        }
    }

    /// The first write failure since the last call.
    pub(crate) const fn take_failure(&mut self) -> Option<io::Error> {
        self.failure.take()
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        match self.writer.as_mut() {
            Some(writer) => writer.flush(),
            None => Ok(()),
        }
    }
}

/// The log a run writes each appended event's line to: it forwards to the caller's log and lets the observer
/// see the event that line is about. It never fails, so a failing caller's log does not stop the observer.
pub(crate) struct Tee {
    pub user: SharedLog,
    pub watch: Option<Watch>,
}

pub(crate) struct Watch {
    pub fs: MemFs,
    pub observer: Slot,
    pub cancel: CancellationToken,
}

impl Write for Tee {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.user.borrow_mut().write(bytes);
        if let Some(watch) = self.watch.as_mut() {
            watch.see(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Watch {
    fn see(&mut self, bytes: &[u8]) {
        for byte in bytes {
            if *byte != b'\n' {
                continue;
            }
            self.appended();
        }
    }

    fn appended(&mut self) {
        let mut slot = self.observer.borrow_mut();
        let Some(observer) = slot.as_mut() else {
            return;
        };
        let Some(event) = self.last_event() else {
            return;
        };
        if observer(&event) == Observe::Stop {
            *slot = None;
            self.cancel.cancel();
        }
    }

    fn last_event(&self) -> Option<Event> {
        let bytes = self.fs.contents(EVENTS)?;
        let text = std::str::from_utf8(&bytes).ok()?;
        let line = text.strip_suffix('\n')?.rsplit('\n').next()?;
        Event::decode(line).ok()
    }
}
