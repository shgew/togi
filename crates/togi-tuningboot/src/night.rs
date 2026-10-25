//! Switching between night and day: the night record, its morning deadline and the boot loader's
//! selection behind the one seam [`Selection`].
//!
//! A night is the tuning boot entered for a bounded time. Its record holds the morning deadline, so
//! a guard that runs early in every tuning boot can end the night once the morning has passed.
//! Nothing else the guard sees ends a night: a failure is the tuner's to handle. Returning to the
//! day restores the selection the night replaced, exactly, and never selects a particular
//! generation.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::str::FromStr;

use jiff::Timestamp;
use jiff::civil::Time;
use jiff::tz::{AmbiguousOffset, TimeZone};
use serde::de::{self, DeserializeOwned};
use serde::{Deserialize, Serialize};

use crate::{Bootloader, SavedEntry};

/// GRUB's variable naming the entry `default=saved` boots.
pub const SAVED_ENTRY_VARIABLE: &str = "saved_entry";
/// The night record, while a night is in progress.
pub const NIGHT_VARIABLE: &str = "togi_night";
/// The `saved_entry` the day had before the night; unset when the day boots the first menu entry.
pub const DAY_ENTRY_VARIABLE: &str = "togi_day_entry";
/// Where the NixOS module writes the [`Setup`].
pub const SETUP_PATH: &str = "/etc/togi/night.json";

/// The longest entry name kept, so the night's variables leave room in GRUB's 1024-byte block.
const MAX_ENTRY_BYTES: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum NightError {
    #[error("read the night record")]
    Read(#[source] io::Error),
    #[error("invalid night record {record:?}")]
    Invalid {
        record: String,
        #[source]
        source: RecordError,
    },
    #[error("read the selected boot entry")]
    ReadSelection(#[source] io::Error),
    #[error("select the tuning entry")]
    Select(#[source] io::Error),
    #[error(
        "the tuning entry {expected:?} is not selected after selecting it: {SAVED_ENTRY_VARIABLE} is {found:?}"
    )]
    NotSelected {
        expected: String,
        found: Option<String>,
    },
    #[error("store the night record")]
    Store(#[source] io::Error),
    #[error("restore the day's boot entry")]
    Restore(#[source] io::Error),
    #[error("forget the night record")]
    Forget(#[source] io::Error),
    #[error("find the next morning")]
    Morning(#[source] jiff::Error),
}

/// Why a night record or a part of the setup was refused.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("record is malformed")]
    Syntax(#[source] serde_json::Error),
    #[error("a time is out of range")]
    Time(#[source] jiff::Error),
    #[error("entry must have 1 to 128 printable ASCII bytes")]
    Entry,
    #[error("morning must be HH:MM, 00:00 to 23:59")]
    Morning,
}

/// A boot entry as the boot loader names it: 1 to 128 printable ASCII bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Entry(String);

impl Entry {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Entry {
    type Error = RecordError;

    fn try_from(value: String) -> Result<Self, RecordError> {
        let printable = value.bytes().all(|byte| (b' '..=b'~').contains(&byte));
        if (1..=MAX_ENTRY_BYTES).contains(&value.len()) && printable {
            Ok(Self(value))
        } else {
            Err(RecordError::Entry)
        }
    }
}

impl From<Entry> for String {
    fn from(entry: Entry) -> Self {
        entry.0
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The time of day a night ends, written `HH:MM` in the machine's time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Morning(Time);

impl Morning {
    pub fn time(self) -> Time {
        self.0
    }

    /// The first instant after `now` at which the clock in `zone` reads this time. A time the zone
    /// skips on that day resolves as jiff's compatible disambiguation does; a time the clock reads
    /// twice counts at both readings.
    ///
    /// # Errors
    ///
    /// The date or the instant is out of jiff's range.
    pub fn next_after(self, now: Timestamp, zone: &TimeZone) -> Result<Timestamp, NightError> {
        let mut date = now.to_zoned(zone.clone()).date();
        loop {
            let ambiguous = zone.to_ambiguous_timestamp(date.to_datetime(self.0));
            let first = ambiguous.compatible().map_err(NightError::Morning)?;
            if first > now {
                return Ok(first);
            }
            if matches!(ambiguous.offset(), AmbiguousOffset::Fold { .. }) {
                let second = ambiguous.later().map_err(NightError::Morning)?;
                if second > now {
                    return Ok(second);
                }
            }
            date = date.tomorrow().map_err(NightError::Morning)?;
        }
    }
}

impl FromStr for Morning {
    type Err = RecordError;

    fn from_str(value: &str) -> Result<Self, RecordError> {
        let (hour, minute) = value.split_once(':').ok_or(RecordError::Morning)?;
        let digits = |part: &str| {
            (part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse::<i8>().ok())
                .flatten()
                .ok_or(RecordError::Morning)
        };
        let time =
            Time::new(digits(hour)?, digits(minute)?, 0, 0).map_err(|_| RecordError::Morning)?;
        Ok(Self(time))
    }
}

impl TryFrom<String> for Morning {
    type Error = RecordError;

    fn try_from(value: String) -> Result<Self, RecordError> {
        value.parse()
    }
}

impl fmt::Display for Morning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.0.hour(), self.0.minute())
    }
}

/// What the NixOS module tells the night switch about the installed tuning boot ([`SETUP_PATH`]).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    /// GRUB's environment block.
    pub grubenv: PathBuf,
    /// The tuning boot's menu entry.
    pub entry: Entry,
    /// When a night ends unless `togi night --until` says otherwise.
    pub morning: Morning,
}

impl FromStr for Setup {
    type Err = RecordError;

    fn from_str(text: &str) -> Result<Self, RecordError> {
        object_only(text).map_err(RecordError::Syntax)
    }
}

/// Decodes `text` only when it is a JSON object: serde's derived `Deserialize` for a struct also
/// accepts an array of the fields in declaration order, which would set them by position.
fn object_only<T: DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    if text.trim_start().starts_with('{') {
        serde_json::from_str(text)
    } else {
        Err(de::Error::custom("expected a JSON object"))
    }
}

/// A night in progress: its morning deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Night {
    until: Timestamp,
}

/// The record as it is stored: Unix seconds.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    until: i64,
}

impl Night {
    /// A night that ends at `until`.
    #[must_use]
    pub fn new(until: Timestamp) -> Self {
        Self { until }
    }

    pub fn until(&self) -> Timestamp {
        self.until
    }

    /// Whether the morning has come at `now`.
    #[must_use]
    pub fn ended(&self, now: Timestamp) -> bool {
        now >= self.until
    }

    fn encode(&self) -> String {
        let wire = Wire {
            until: self.until.as_second(),
        };
        // A struct of one integer always serializes.
        serde_json::to_string(&wire).unwrap_or_default()
    }

    fn decode(record: &str) -> Result<Self, RecordError> {
        let wire: Wire = object_only(record).map_err(RecordError::Syntax)?;
        Ok(Self {
            until: Timestamp::from_second(wire.until).map_err(RecordError::Time)?,
        })
    }
}

/// The boot loader's selection of the next boot, as the night switch uses it. This is the one seam
/// a boot loader implements: [`GrubSelection`] keeps everything in GRUB's environment block.
pub trait Selection {
    /// The night in progress, if one is recorded.
    ///
    /// # Errors
    ///
    /// The record cannot be read or is invalid.
    fn night(&mut self) -> Result<Option<Night>, NightError>;

    /// Selects `tuning` for every following boot and records `night`, remembering the selection it
    /// replaces. Beginning again while `tuning` is already selected keeps the remembered selection.
    ///
    /// # Errors
    ///
    /// The selection cannot be read or written, or does not read back as `tuning`.
    fn begin(&mut self, tuning: &Entry, night: &Night) -> Result<(), NightError>;

    /// Records `night` for a tuning boot entered without [`Selection::begin`]: its day selection is
    /// unknown, so the day returns to the boot loader's default.
    ///
    /// # Errors
    ///
    /// The record cannot be written.
    fn adopt(&mut self, night: &Night) -> Result<(), NightError>;

    /// Records `night` over the one in progress.
    ///
    /// # Errors
    ///
    /// The record cannot be written.
    fn store(&mut self, night: &Night) -> Result<(), NightError>;

    /// Restores the day's selection exactly and forgets the night: [`Bootloader::clear_saved_entry`],
    /// which a boot loader implements by restoring what [`Selection::begin`] remembered. Repeating it
    /// selects the same day entry again.
    ///
    /// # Errors
    ///
    /// The selection cannot be restored or does not read back as the day's.
    fn end(&mut self) -> Result<SavedEntry, NightError>;
}

/// [`Selection`] in GRUB's environment block: `saved_entry` is the selection, `togi_day_entry` the
/// one the night replaced and `togi_night` the record.
pub struct GrubSelection(Box<dyn Bootloader>);

impl GrubSelection {
    #[must_use]
    pub fn new(bootloader: Box<dyn Bootloader>) -> Self {
        Self(bootloader)
    }

    fn saved(&mut self) -> Result<Option<String>, NightError> {
        self.0
            .get(SAVED_ENTRY_VARIABLE)
            .map(|value| value.filter(|value| !value.is_empty()))
            .map_err(NightError::ReadSelection)
    }
}

impl Selection for GrubSelection {
    fn night(&mut self) -> Result<Option<Night>, NightError> {
        match self.0.get(NIGHT_VARIABLE).map_err(NightError::Read)? {
            Some(record) if !record.is_empty() => Night::decode(&record)
                .map(Some)
                .map_err(|source| NightError::Invalid { record, source }),
            _ => Ok(None),
        }
    }

    fn begin(&mut self, tuning: &Entry, night: &Night) -> Result<(), NightError> {
        let record = night.encode();
        let written = match self.saved()? {
            Some(current) if current == tuning.as_str() => self.0.set(&[(NIGHT_VARIABLE, &record)]),
            Some(day) => self.0.set(&[
                (SAVED_ENTRY_VARIABLE, tuning.as_str()),
                (DAY_ENTRY_VARIABLE, &day),
                (NIGHT_VARIABLE, &record),
            ]),
            None => self.0.unset(&[DAY_ENTRY_VARIABLE]).and_then(|()| {
                self.0.set(&[
                    (SAVED_ENTRY_VARIABLE, tuning.as_str()),
                    (NIGHT_VARIABLE, &record),
                ])
            }),
        };
        written.map_err(NightError::Select)?;
        let found = self.saved()?;
        if found.as_deref() == Some(tuning.as_str()) {
            Ok(())
        } else {
            Err(NightError::NotSelected {
                expected: tuning.to_string(),
                found,
            })
        }
    }

    fn adopt(&mut self, night: &Night) -> Result<(), NightError> {
        self.0
            .unset(&[DAY_ENTRY_VARIABLE])
            .map_err(NightError::Store)?;
        self.store(night)
    }

    fn store(&mut self, night: &Night) -> Result<(), NightError> {
        self.0
            .set(&[(NIGHT_VARIABLE, &night.encode())])
            .map_err(NightError::Store)
    }

    fn end(&mut self) -> Result<SavedEntry, NightError> {
        self.0.clear_saved_entry().map_err(NightError::Restore)
    }
}

/// What the early-boot guard decided for this tuning boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// The night goes on.
    Tune(Night),
    /// The morning has come and the day's selection is restored; the caller reboots.
    Return(Night),
}

/// The early-boot guard of a tuning boot, before the tuner starts: returns to the day when the
/// morning has come and otherwise lets the tuner start, however many boots the night has had. A
/// tuning boot without a night record was entered from the boot menu: it gets a night ending at the
/// next `morning` in `zone`, and the day returns to the boot loader's default. The guard needs
/// neither the tuner nor its journal.
///
/// # Errors
///
/// Reading or writing the selection failed.
pub fn guard(
    selection: &mut (impl Selection + ?Sized),
    now: Timestamp,
    morning: Morning,
    zone: &TimeZone,
) -> Result<Guard, NightError> {
    match selection.night() {
        Ok(Some(night)) if night.ended(now) => {
            selection.end()?;
            Ok(Guard::Return(night))
        }
        Ok(Some(night)) => Ok(Guard::Tune(night)),
        Ok(None) => {
            let night = Night::new(morning.next_after(now, zone)?);
            selection.adopt(&night)?;
            Ok(Guard::Tune(night))
        }
        // A record the guard cannot read would otherwise hold the machine in the tuning boot with no tuner:
        // it is replaced by a night to the next morning, which keeps the day the night remembered.
        Err(NightError::Invalid { .. }) => {
            let night = Night::new(morning.next_after(now, zone)?);
            selection.store(&night)?;
            Ok(Guard::Tune(night))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
