use jiff::{SignedDuration, Timestamp};
use std::collections::HashMap;
use std::path::Path;
use togi_machine::{BootId, CcdId, PerCore};

use togi_journal::{Event, Payload, Recording, Skipped};

use crate::error::Error;

/// A session's events, read whole without taking the lock or writing anything. A live journal's unfinished
/// final line is ignored, events of kinds this build does not know are skipped and unknown fields ignored
/// ([`skipped`](Self::skipped) says what was).
pub struct Journal {
    events: Vec<Event>,
    ccds: PerCore<Option<CcdId>>,
    skipped: Skipped,
    boot_starts: HashMap<BootId, Timestamp>,
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
        let ccds = start
            .cores
            .iter()
            .map(|info| (info.core, Some(info.ccd)))
            .collect();
        let mut boot_starts = HashMap::new();
        for event in &events {
            let requests = if let Payload::TrialEnd(end) = &event.payload {
                Some(&end.voltage_requests_v)
            } else if let Payload::TrialCarried(carried) = &event.payload {
                Some(&carried.voltage_requests_v)
            } else {
                None
            };
            if let Some(requests) = requests {
                for (_, request) in requests.iter() {
                    crate::requests::checked_bin(request)?;
                }
            }
            if !boot_starts.contains_key(&event.boot) {
                let start = match event.mono {
                    Some(mono) => {
                        let up = SignedDuration::try_from(mono).map_err(Error::BootDuration)?;
                        event.time.checked_sub(up).map_err(Error::BootStart)?
                    }
                    None => event.time,
                };
                boot_starts.insert(event.boot.clone(), start);
            }
        }
        Ok(Self {
            events,
            ccds,
            skipped,
            boot_starts,
        })
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// What reading the journal skipped or ignored; empty for one built from events.
    pub const fn skipped(&self) -> &Skipped {
        &self.skipped
    }

    /// The CCD `session.start` records for `core`, if it lists the core.
    pub(crate) fn ccd_of(&self, core: togi_machine::CoreId) -> Option<CcdId> {
        self.ccds.get(core).flatten()
    }

    pub(crate) fn boot_start(&self, boot: &BootId) -> Timestamp {
        self.boot_starts[boot]
    }
}
