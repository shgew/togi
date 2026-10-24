//! `togi day record`: journaling a crash no day observer saw.
//!
//! A [`Draft`] is what the user said, however they said it; [`draft_record`] checks it against the journal and
//! becomes the [`DayRecord`] that [`append_record`] writes.

use jiff::Timestamp;
use togi_journal::{
    AppendError, CrashedBoot, DayCrashes, DayRecord, Event, Journal, OffsetSource, OffsetsError,
    Payload, RecordedOffsets, Running, Seq, Stamp,
};
use togi_machine::{BootId, CcdId, Clock, CoreId, CoreInfo, ObservedOffset, PerCore};

/// One offset as a user gives it. A more specific spec overrides a less specific one; two specs of the same kind
/// for one core are refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OffsetSpec {
    /// Every core of the session.
    All(i64),
    /// Every core of one CCD.
    Ccd(CcdId, i64),
    /// One core.
    Core(CoreId, i64),
}

/// Which kind of spec set a core, from the least specific to the most.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Specificity {
    All,
    Ccd,
    Core,
}

impl OffsetSpec {
    const fn specificity(self) -> Specificity {
        match self {
            Self::All(_) => Specificity::All,
            Self::Ccd(..) => Specificity::Ccd,
            Self::Core(..) => Specificity::Core,
        }
    }
}

/// Offsets the user typed in rather than took from a day observer's reading, and when they entered them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entered {
    pub at: Timestamp,
    pub specs: Vec<OffsetSpec>,
}

/// Which crash the user is recording, and where its offsets come from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Crash {
    /// A boot with a recorded unclean end. Without `entered`, its offsets are the ones the observer read back.
    Observed {
        boot: BootId,
        entered: Option<Entered>,
    },
    /// A crash no observer saw: it has no read-back offsets, so the user enters them.
    Unobserved { at: Timestamp, entered: Entered },
}

/// What a user told `togi day record`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Draft {
    pub crash: Crash,
    pub running: Running,
    pub note: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("no session in the journal")]
    NoSession,
    #[error("boot {boot} has no recorded unclean end")]
    UnknownBoot { boot: BootId },
    #[error("boot {boot} already has a crash record, at seq {seq}")]
    AlreadyRecorded { boot: BootId, seq: Seq },
    #[error("boot {boot}: its observer's last offset reading is not in the journal")]
    NoReadBack { boot: BootId },
    #[error("core {core:02} is not in the session")]
    NoSuchCore { core: CoreId },
    #[error("CCD {} has no core in the session", ccd.get())]
    NoSuchCcd { ccd: CcdId },
    #[error("core {core:02} is given an offset twice by the same kind of option")]
    Duplicate { core: CoreId },
    #[error("the offsets leave core {core:02} without a value; give every core of the session one")]
    Missing { core: CoreId },
    #[error(transparent)]
    Offsets(#[from] OffsetsError),
}

#[derive(Debug, thiserror::Error)]
pub enum AppendRecordError {
    #[error("append the crash record")]
    Append(#[from] AppendError),
    #[error("read the record event clock")]
    Clock(#[from] togi_machine::Error),
}

/// The cores of the session's first `session.start`.
fn topology(events: &[Event]) -> Result<&[CoreInfo], RecordError> {
    events
        .iter()
        .find_map(|event| {
            if let Payload::SessionStart(start) = &event.payload {
                Some(start.cores.as_slice())
            } else {
                None
            }
        })
        .ok_or(RecordError::NoSession)
}

fn resolve(specs: &[OffsetSpec], cores: &[CoreInfo]) -> Result<RecordedOffsets, RecordError> {
    let mut specs = specs.to_vec();
    specs.sort_by_key(|spec| spec.specificity());
    let mut offsets = PerCore::<ObservedOffset>::default();
    let mut given = PerCore::<Option<Specificity>>::default();
    let mut give = |core: CoreId, value: i64, by: Specificity| {
        if given.get(core).flatten() == Some(by) {
            return Err(RecordError::Duplicate { core });
        }
        offsets.set(core, ObservedOffset(value));
        given.set(core, Some(by));
        Ok(())
    };
    for spec in specs {
        let by = spec.specificity();
        match spec {
            OffsetSpec::All(value) => {
                for info in cores {
                    give(info.core, value, by)?;
                }
            }
            OffsetSpec::Ccd(ccd, value) => {
                let mut any = false;
                for info in cores.iter().filter(|info| info.ccd == ccd) {
                    give(info.core, value, by)?;
                    any = true;
                }
                if !any {
                    return Err(RecordError::NoSuchCcd { ccd });
                }
            }
            OffsetSpec::Core(core, value) => {
                if !cores.iter().any(|info| info.core == core) {
                    return Err(RecordError::NoSuchCore { core });
                }
                give(core, value, by)?;
            }
        }
    }
    if let Some(info) = cores.iter().find(|info| offsets.get(info.core).is_none()) {
        // An empty answer is its own refusal: a record that names no offsets.
        if offsets.is_empty() {
            return Err(OffsetsError::Empty.into());
        }
        return Err(RecordError::Missing { core: info.core });
    }
    Ok(RecordedOffsets::new(offsets)?)
}

/// Checks `draft` against the journal and returns the record it describes.
///
/// # Errors
/// Refuses a boot with no recorded unclean end or one that already has a record, a core or CCD the session does
/// not have, a core given twice, a core left without an offset when the user enters them, offsets that name no
/// core and an offset outside what togi applies.
pub fn draft_record(events: &[Event], draft: &Draft) -> Result<DayRecord, RecordError> {
    let cores = topology(events)?;
    let (boot, offsets, source) = match &draft.crash {
        Crash::Observed { boot, entered } => {
            let crashes = DayCrashes::of(events);
            let named = crashes
                .get(boot)
                .ok_or_else(|| RecordError::UnknownBoot { boot: boot.clone() })?;
            if let Some(seq) = named.recorded {
                return Err(RecordError::AlreadyRecorded {
                    boot: boot.clone(),
                    seq,
                });
            }
            let (offsets, source) = match entered {
                Some(entered) => (
                    resolve(&entered.specs, cores)?,
                    OffsetSource::Entered { at: entered.at },
                ),
                None => {
                    let at = named
                        .read_back
                        .ok_or_else(|| RecordError::NoReadBack { boot: boot.clone() })?;
                    (
                        RecordedOffsets::new(named.end.offsets)?,
                        OffsetSource::ReadBack { at },
                    )
                }
            };
            (
                CrashedBoot::UncleanEnd { boot: boot.clone() },
                offsets,
                source,
            )
        }
        Crash::Unobserved { at, entered } => (
            CrashedBoot::Unobserved { at: *at },
            resolve(&entered.specs, cores)?,
            OffsetSource::Entered { at: entered.at },
        ),
    };
    Ok(DayRecord {
        boot,
        offsets,
        source,
        running: draft.running,
        note: draft.note.clone(),
    })
}

/// Appends `record` and returns its sequence number.
///
/// # Errors
/// The event clock could not be read, or the append failed; nothing is appended on a clock failure.
pub fn append_record(
    journal: &mut Journal,
    clock: &dyn Clock,
    record: DayRecord,
) -> Result<Seq, AppendRecordError> {
    let stamp = Stamp {
        time: clock.now(),
        mono: Some(clock.monotonic()?),
    };
    Ok(journal.append(stamp, Payload::DayRecord(record), &[])?.seq)
}

#[cfg(test)]
mod tests;
