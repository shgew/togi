use std::path::Path;

use togi_journal::{Event, Payload, Recording, Skipped};
use togi_tuner::Topology;

use crate::error::Error;

/// A session's events, read whole without taking the lock or writing anything. A live journal's unfinished
/// final line is ignored, events of kinds this build does not know are skipped and unknown fields ignored
/// ([`skipped`](Self::skipped) says what was).
pub struct Journal {
    events: Vec<Event>,
    topology: Topology,
    skipped: Skipped,
}

impl Journal {
    pub fn read(path: impl AsRef<Path>) -> Result<Self, Error> {
        let recording = Recording::read_lenient(path.as_ref())?;
        let skipped = recording.skipped().clone();
        Self::with_skipped(recording.into_events(), skipped)
    }

    /// A journal of `events`, which must open with `session.start` and number their events upward from 1, as the
    /// reader guarantees of a file; a lenient read leaves holes where it skipped events.
    pub fn new(events: Vec<Event>) -> Result<Self, Error> {
        Self::with_skipped(events, Skipped::default())
    }

    fn with_skipped(events: Vec<Event>, skipped: Skipped) -> Result<Self, Error> {
        let Some(first) = events.first() else {
            return Err(Error::Empty);
        };
        let Payload::SessionStart(start) = &first.payload else {
            return Err(Error::NotStarted);
        };
        let topology = Topology::new(&start.cores)?;
        Ok(Self {
            events,
            topology,
            skipped,
        })
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// What reading the journal skipped or ignored; empty for one built from events.
    pub const fn skipped(&self) -> &Skipped {
        &self.skipped
    }

    pub(crate) fn topology(&self) -> &Topology {
        &self.topology
    }
}
