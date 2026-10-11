use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use togi_journal::{Event, MemFs};
use togi_machine::CancellationToken;

use crate::input::{Observe, Observer};

pub(crate) type UserLog = Rc<RefCell<Option<Box<dyn Write>>>>;
pub(crate) type Slot = Rc<RefCell<Option<Observer>>>;

const EVENTS: &str = "/state/events.jsonl";

/// The log a run writes each appended event's line to: it forwards to the caller's log and lets the observer
/// see the event that line is about.
pub(crate) struct Tee {
    pub user: UserLog,
    pub watch: Option<Watch>,
}

pub(crate) struct Watch {
    pub fs: MemFs,
    pub observer: Slot,
    pub cancel: CancellationToken,
}

impl Write for Tee {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(log) = self.user.borrow_mut().as_mut() {
            log.write_all(bytes)?;
        }
        if let Some(watch) = self.watch.as_mut() {
            watch.see(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.user.borrow_mut().as_mut() {
            Some(log) => log.flush(),
            None => Ok(()),
        }
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
