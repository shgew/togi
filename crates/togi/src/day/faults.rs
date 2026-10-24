//! Journals the faults the running boot's kernel log reports, each once ([Live faults](../../../../docs/spec/runtime.md#live-faults)).

use std::collections::HashMap;

use togi_hardware::day::Observer;
use togi_journal::{DayFault, Event, Journal, LiveFault, Payload, Trap};
use togi_machine::{BootId, Clock, KernelCursor, KernelRead, ObservedOffset, PerCore};

use super::{Error, append};

/// How far apart the kernel line and the core dump record of one killed process may be logged and still be one
/// fault. Writing the dump of a large process takes tens of seconds; a pid is not handed out again to the same
/// executable within a minute short of tens of thousands of forks.
const TRAP_WINDOW_NS: u64 = 60_000_000_000;

/// The faults a boot has journaled, by identity ([`DayFault::identity`]) and log time.
#[derive(Default)]
struct Seen(HashMap<LiveFault, Vec<u64>>);

impl Seen {
    /// Notes a fault and says whether it is new. A trap matches an earlier one of its identity logged within
    /// [`TRAP_WINDOW_NS`]; any other fault matches only the same log time.
    fn insert(&mut self, (identity, at): (LiveFault, u64)) -> bool {
        let window = match identity {
            LiveFault::Trap { .. } => TRAP_WINDOW_NS,
            LiveFault::MachineCheck { .. } | LiveFault::KernelError { .. } => 0,
        };
        let times = self.0.entry(identity).or_default();
        let new = times.iter().all(|seen| seen.abs_diff(at) > window);
        if new {
            times.push(at);
        }
        new
    }
}

/// What the observer has already journaled of its boot, and where in the log it read to.
pub(super) struct Faults {
    cursor: Option<KernelCursor>,
    seen: Seen,
}

impl Faults {
    /// Starts from the faults the journal already holds for `boot`, which a restart of the service within the
    /// boot must not journal again. The first poll reads the whole boot; the later ones resume after the cursor.
    pub(super) fn of(events: &[Event], boot: &BootId) -> Self {
        let mut seen = Seen::default();
        for event in events.iter().filter(|event| &event.boot == boot) {
            if let Payload::DayFault(fault) = &event.payload {
                seen.insert(fault.identity());
            }
        }
        Self { cursor: None, seen }
    }

    /// Journals each fault the log holds that the journal does not, oldest first, with the `offsets` last read
    /// back. A read that fails journals what it did read before it returns the failure. A cursor the log no longer
    /// holds is not a failure: the boot is read again from its start within the same poll, once, so a poll never
    /// returns with faults it could have read still in the log.
    pub(super) fn poll(
        &mut self,
        journal: &mut Journal,
        observer: &mut dyn Observer,
        clock: &dyn Clock,
        offsets: PerCore<ObservedOffset>,
    ) -> Result<(), Error> {
        let after = self.cursor.take();
        let resumed = after.is_some();
        let cursor = match self.read(journal, observer, clock, offsets, after.as_ref()) {
            Err(Error::Observe(togi_machine::Error::CursorMissing)) if resumed => {
                self.read(journal, observer, clock, offsets, None)?
            }
            other => other?,
        };
        self.cursor = Some(cursor);
        Ok(())
    }

    /// One read of the log after `after`, journaled; the cursor to resume from.
    fn read(
        &mut self,
        journal: &mut Journal,
        observer: &mut dyn Observer,
        clock: &dyn Clock,
        offsets: PerCore<ObservedOffset>,
        after: Option<&KernelCursor>,
    ) -> Result<KernelCursor, Error> {
        let (read, failure) = match observer.live_faults(journal.boot(), after) {
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
        failure.map_or(Ok(cursor), |error| Err(Error::Observe(error)))
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
