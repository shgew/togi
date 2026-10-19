use jiff::{SignedDuration, Timestamp};
use std::collections::HashMap;
use std::path::Path;
use togi_machine::BootId;

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
        let topology = Topology::new(&start.cores)?;
        let mut boot_starts = HashMap::new();
        for event in &events {
            if let Payload::CheckingCycle(cycle) = &event.payload {
                checking_steps(cycle.steps.len())?;
            }
            let requests = if let Payload::TrialEnd(end) = &event.payload {
                Some(&end.voltage_requests_v)
            } else if let Payload::TrialCarried(carried) = &event.payload {
                Some(&carried.voltage_requests_v)
            } else {
                None
            };
            if let Some(requests) = requests {
                for (_, request) in requests.iter() {
                    crate::r7::checked_bin(request)?;
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
            topology,
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

    pub(crate) fn topology(&self) -> &Topology {
        &self.topology
    }

    pub(crate) fn boot_start(&self, boot: &BootId) -> Timestamp {
        self.boot_starts[boot]
    }
}

fn checking_steps(count: usize) -> Result<u32, Error> {
    u32::try_from(count).map_err(Error::CheckingSteps)
}

#[cfg(test)]
mod numeric_tests {
    use super::checking_steps;

    #[test_case::test_case(0, Some(0); "empty steps")]
    #[test_case::test_case(u32::MAX as usize, Some(u32::MAX); "last exact ordinal")]
    #[test_case::test_case(u32::MAX as usize + 1, None; "overflowing ordinal")]
    #[cfg(target_pointer_width = "64")]
    fn checking_step_counts_are_checked(count: usize, expected: Option<u32>) {
        assert_eq!(checking_steps(count).ok(), expected);
    }
}
