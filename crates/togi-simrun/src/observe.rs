use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::Event;
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::AppendHook;

use crate::input::{Observe, Observer};

pub(crate) type SharedLog = Rc<RefCell<UserLog>>;
pub(crate) type Slot = Rc<RefCell<Option<Observer>>>;

/// The caller's log. The first write that fails drops it and is kept for the simulation to report, so the
/// observer keeps seeing every appended event.
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

/// The log a run writes each appended event's line to: it forwards to the caller's log and never fails, so a
/// failing caller's log does not stop the run's other consumers.
pub(crate) struct Tee(pub SharedLog);

impl Write for Tee {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().write(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The hook that shows `slot`'s observer each event a run appends. An observer that answers
/// [`Observe::Stop`] is dropped and `cancel` fires, so it is never called again. The operator's `stop_at` fires
/// `cancel` at the first event at or after it.
pub(crate) fn hook(
    slot: Slot,
    cancel: CancellationToken,
    stop_at: Option<Timestamp>,
) -> AppendHook {
    Box::new(move |event| {
        let mut slot = slot.borrow_mut();
        if let Some(observer) = slot.as_mut()
            && observer(event) == Observe::Stop
        {
            *slot = None;
            cancel.cancel();
        }
        if stop_at.is_some_and(|stop_at| event.time >= stop_at) {
            cancel.cancel();
        }
    })
}
