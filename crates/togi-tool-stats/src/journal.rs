use std::path::Path;

use togi_journal::{Event, Payload, Recording};
use togi_tuner::Topology;

use crate::error::Error;

/// A session's events, read whole without taking the lock or writing anything. A live journal's unfinished
/// final line is ignored.
pub struct Journal {
    events: Vec<Event>,
    topology: Topology,
}

impl Journal {
    pub fn read(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::new(Recording::read(path.as_ref())?.into_events())
    }

    /// A journal of `events`, which must open with `session.start` and number their events from 1, as the
    /// reader guarantees of a file.
    pub fn new(events: Vec<Event>) -> Result<Self, Error> {
        let Some(first) = events.first() else {
            return Err(Error::Empty);
        };
        let Payload::SessionStart(start) = &first.payload else {
            return Err(Error::NotStarted);
        };
        let topology = Topology::new(&start.cores)?;
        Ok(Self { events, topology })
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub(crate) fn topology(&self) -> &Topology {
        &self.topology
    }
}
