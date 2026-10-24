//! Journals the faults the running boot's kernel log reports, each once ([Live faults](../../../../docs/spec/runtime.md#live-faults)).

use std::collections::HashSet;

use togi_hardware::day::Observer;
use togi_journal::{DayFault, Event, Journal, LiveFault, Payload, Trap};
use togi_machine::{BootId, Clock, KernelCursor, KernelRead, ObservedOffset, PerCore};

use super::{Error, append};

/// What the observer has already journaled of its boot, and where in the log it read to.
pub(super) struct Faults {
    cursor: Option<KernelCursor>,
    seen: HashSet<(LiveFault, u64)>,
}

impl Faults {
    /// Starts from the faults the journal already holds for `boot`, which a restart of the service within the
    /// boot must not journal again. The first poll reads the whole boot; the later ones resume after the cursor.
    pub(super) fn of(events: &[Event], boot: &BootId) -> Self {
        let seen = events
            .iter()
            .filter(|event| &event.boot == boot)
            .filter_map(|event| match &event.payload {
                Payload::DayFault(fault) => Some(fault.identity()),
                _ => None,
            })
            .collect();
        Self { cursor: None, seen }
    }

    /// Journals each fault the log holds that the journal does not, oldest first, with the `offsets` last read
    /// back. A read that fails journals what it did read before it returns the failure; an unresumable cursor is
    /// not a failure but a reason to read the boot again from its start.
    pub(super) fn poll(
        &mut self,
        journal: &mut Journal,
        observer: &mut dyn Observer,
        clock: &dyn Clock,
        offsets: PerCore<ObservedOffset>,
    ) -> Result<(), Error> {
        let (read, failure) = match observer.live_faults(journal.boot(), self.cursor.as_ref()) {
            Ok(read) => (read, None),
            Err(partial) => {
                let (read, error) = partial.into_parts();
                (read, Some(error))
            }
        };
        let cursor = read.cursor.clone();
        for fault in found(read)? {
            if self.seen.insert(fault.identity()) {
                append(journal, clock, DayFault { offsets, ..fault })?;
            }
        }
        match failure {
            None => self.cursor = Some(cursor),
            Some(togi_machine::Error::CursorMissing) => self.cursor = None,
            Some(error) => return Err(Error::Observe(error)),
        }
        Ok(())
    }
}

/// The recordable faults of one read in the order the kernel logged them, with an empty `offsets` to fill in.
fn found(read: KernelRead) -> Result<Vec<DayFault>, Error> {
    let nanos = |time: std::time::Duration| {
        u64::try_from(time.as_nanos()).map_err(|_| Error::DurationOverflow)
    };
    let mut found = Vec::new();
    for mce in read.mces {
        found.push(DayFault {
            fault: LiveFault::MachineCheck {
                cpu: mce.cpu,
                core: mce.core,
                bank: mce.bank,
                bank_type: mce.bank_type,
                corrected: mce.corrected,
            },
            monotonic_ns: nanos(mce.monotonic)?,
            offsets: PerCore::default(),
            lines: mce.lines,
        });
    }
    for error in read.errors {
        found.push(DayFault {
            fault: LiveFault::KernelError { error: error.kind },
            monotonic_ns: nanos(error.monotonic)?,
            offsets: PerCore::default(),
            lines: error.lines,
        });
    }
    for process in read.faults {
        let Some(trap) = Trap::of(process.kind) else {
            continue;
        };
        found.push(DayFault {
            fault: LiveFault::Trap {
                trap,
                name: process.name,
                pid: process.pid,
                cpu: process.cpu,
                core: process.core,
            },
            monotonic_ns: nanos(process.monotonic)?,
            offsets: PerCore::default(),
            lines: process.lines,
        });
    }
    found.sort_by_key(|fault| fault.monotonic_ns);
    Ok(found)
}
