//! Bounded restart-limit retries and leave reasons kept in GRUB's environment block, and the night
//! switch ([`night`]): the boot loader selection between night and day.
//!
//! The record types make an invalid leave reason unrepresentable, and `restart_limit` records an
//! attempt before it grants either boot action, so a failure never grants a retry.

pub mod night;

use std::fmt;
use std::io;
use std::str::FromStr;

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

pub const COUNT_VARIABLE: &str = "togi_restart_count";
pub const REASON_VARIABLE: &str = "togi_leave_reason";
pub const RETRY_MARKER: &str = "/run/togi-retry-tuning-boot";

const RETRY_REASON: &str = "service restart limit hit; rebooting back into the tuning boot";
const EXHAUSTED_REASON: &str = "service restart limit exhausted; returning to the normal system";
const MAX_RECORD_BYTES: usize = 300;
const MAX_ID_BYTES: usize = 64;
const MAX_REASON_BYTES: usize = 160;
const MAX_COUNT: u8 = 3;
const HEX: &[u8; 16] = b"0123456789abcdef";
const RECORD_OVERHEAD: usize = r#"{"id":"","count":0,"reason":""}"#.len();

/// GRUB's environment block. `set` writes all pairs in one operation.
pub trait Environment {
    /// `None` means the variable is unset.
    fn get(&mut self, name: &str) -> io::Result<Option<String>>;
    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()>;
    fn unset(&mut self, names: &[&str]) -> io::Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedEntry {
    pub before: String,
    pub after: String,
}

pub trait Bootloader: Environment {
    /// Selects the tuning entry persistently, remembering the selection it replaces and reading
    /// the result back. The default implements GRUB's environment-block selection.
    fn select_tuning_entry(&mut self, tuning: &night::Entry) -> Result<(), night::NightError> {
        use night::{DAY_ENTRY_VARIABLE, NIGHT_BEGUN, NIGHT_VARIABLE, SAVED_ENTRY_VARIABLE};
        let current = self
            .get(SAVED_ENTRY_VARIABLE)
            .map_err(night::NightError::ReadSelection)?
            .filter(|value| !value.is_empty());
        match current {
            Some(current) if current == tuning.as_str() => return Ok(()),
            Some(day) => self.set(&[
                (SAVED_ENTRY_VARIABLE, tuning.as_str()),
                (DAY_ENTRY_VARIABLE, &day),
                (NIGHT_VARIABLE, NIGHT_BEGUN),
            ]),
            None => self.unset(&[DAY_ENTRY_VARIABLE]).and_then(|()| {
                self.set(&[
                    (SAVED_ENTRY_VARIABLE, tuning.as_str()),
                    (NIGHT_VARIABLE, NIGHT_BEGUN),
                ])
            }),
        }
        .map_err(night::NightError::Select)?;
        let found = self
            .get(SAVED_ENTRY_VARIABLE)
            .map_err(night::NightError::ReadSelection)?
            .filter(|value| !value.is_empty());
        if found.as_deref() != Some(tuning.as_str()) {
            return Err(night::NightError::NotSelected {
                expected: tuning.to_string(),
                found,
            });
        }
        Ok(())
    }

    /// Gives up the tuning selection, so the next boot is the day's. While a night begun with
    /// `night::Selection::begin` lasts (`night::NIGHT_VARIABLE`), the entry it remembered
    /// (`night::DAY_ENTRY_VARIABLE`) is selected again exactly, or none when the day had none, and
    /// the night ends; the remembered entry stays, so a repeat after that changes nothing. Otherwise a
    /// remembered entry belongs to an earlier night and is dropped with the selection, so the boot
    /// loader's default boots. Reads the selection back and reports what it was before and is now.
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry>;
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("read restart count")]
    ReadCount(#[source] io::Error),
    #[error("invalid restart count {0:?}: must be 0 to 3")]
    InvalidCount(String),
    #[error("write restart count and leave reason")]
    WriteCountAndReason(#[source] io::Error),
    #[error("clear GRUB saved entry")]
    ClearSavedEntry(#[source] io::Error),
    #[error("read leave reason")]
    ReadReason(#[source] io::Error),
    #[error("write leave reason")]
    WriteReason(#[source] io::Error),
    #[error("clear leave reason")]
    ClearReason(#[source] io::Error),
    #[error("reset restart count")]
    ResetCount(#[source] io::Error),
    #[error("invalid leave reason record")]
    InvalidRecord(#[source] RecordError),
    #[error("create leave reason ID")]
    Random(#[source] getrandom::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("ID must have 1 to 64 bytes of ASCII letters, digits, '.', '_' or '-'")]
    Id,
    #[error("count must be 0 to 3")]
    Count,
    #[error("reason must have 1 to 160 printable ASCII bytes and no leading or trailing space")]
    Text,
    #[error("record exceeds 300 bytes")]
    TooLong,
    #[error("record must be printable ASCII")]
    NotPrintable,
    #[error("record is malformed")]
    Syntax(#[source] serde_json::Error),
}

/// 1 to 64 bytes of ASCII letters, digits, `.`, `_` and `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReasonId(String);

impl ReasonId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(value: &str) -> Result<(), RecordError> {
        let safe = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        if (1..=MAX_ID_BYTES).contains(&value.len()) && value.bytes().all(safe) {
            Ok(())
        } else {
            Err(RecordError::Id)
        }
    }
}

impl TryFrom<String> for ReasonId {
    type Error = RecordError;

    fn try_from(value: String) -> Result<Self, RecordError> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl FromStr for ReasonId {
    type Err = RecordError;

    fn from_str(value: &str) -> Result<Self, RecordError> {
        Self::validate(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl From<ReasonId> for String {
    fn from(id: ReasonId) -> Self {
        id.0
    }
}

impl fmt::Display for ReasonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How many restart-limit hits happened without durable progress, 0 to 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct RestartCount(u8);

impl RestartCount {
    pub const ZERO: Self = Self(0);

    pub fn get(self) -> u8 {
        self.0
    }

    #[must_use]
    pub fn next(self) -> Self {
        Self((self.0 + 1).min(MAX_COUNT))
    }
}

impl TryFrom<u8> for RestartCount {
    type Error = RecordError;

    fn try_from(value: u8) -> Result<Self, RecordError> {
        if value <= MAX_COUNT {
            Ok(Self(value))
        } else {
            Err(RecordError::Count)
        }
    }
}

impl From<RestartCount> for u8 {
    fn from(count: RestartCount) -> Self {
        count.0
    }
}

/// 1 to 160 printable ASCII bytes, starting and ending with a non-space.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReasonText(String);

impl ReasonText {
    /// Newline, carriage return and tab become spaces, other non-printable or non-ASCII
    /// characters become `?`, and the text is cut to 160 bytes and trimmed. `None` if nothing is
    /// left.
    pub fn sanitize(text: &str) -> Option<Self> {
        let mut cleaned: String = text
            .chars()
            .take(MAX_REASON_BYTES)
            .map(|c| match c {
                '\n' | '\r' | '\t' => ' ',
                ' '..='~' => c,
                _ => '?',
            })
            .collect();
        cleaned.truncate(cleaned.trim_end_matches(' ').len());
        let leading = cleaned.len() - cleaned.trim_start_matches(' ').len();
        cleaned.drain(..leading);
        (!cleaned.is_empty()).then_some(Self(cleaned))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(value: &str) -> Result<(), RecordError> {
        let printable = value.bytes().all(|byte| (b' '..=b'~').contains(&byte));
        let bounded = (1..=MAX_REASON_BYTES).contains(&value.len());
        if printable && bounded && !value.starts_with(' ') && !value.ends_with(' ') {
            Ok(())
        } else {
            Err(RecordError::Text)
        }
    }
}

impl TryFrom<String> for ReasonText {
    type Error = RecordError;

    fn try_from(value: String) -> Result<Self, RecordError> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl FromStr for ReasonText {
    type Err = RecordError;

    fn from_str(value: &str) -> Result<Self, RecordError> {
        Self::validate(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl From<ReasonText> for String {
    fn from(text: ReasonText) -> Self {
        text.0
    }
}

impl fmt::Display for ReasonText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The record in `togi_leave_reason`: why a tuning boot returned to the normal system.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct LeaveReason {
    pub id: ReasonId,
    pub count: RestartCount,
    pub reason: ReasonText,
}

/// Decodes a JSON object only: a positional array, unknown, missing and duplicate fields are
/// all rejected, and each field goes through its validating type.
impl<'de> Deserialize<'de> for LeaveReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        const FIELDS: &[&str] = &["id", "count", "reason"];

        struct RecordVisitor;

        impl<'de> Visitor<'de> for RecordVisitor {
            type Value = LeaveReason;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a leave reason object with id, count and reason")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<LeaveReason, A::Error> {
                let mut id = None;
                let mut count = None;
                let mut reason = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "id" if id.is_none() => id = Some(map.next_value()?),
                        "count" if count.is_none() => count = Some(map.next_value()?),
                        "reason" if reason.is_none() => reason = Some(map.next_value()?),
                        "id" => return Err(de::Error::duplicate_field("id")),
                        "count" => return Err(de::Error::duplicate_field("count")),
                        "reason" => return Err(de::Error::duplicate_field("reason")),
                        other => return Err(de::Error::unknown_field(other, FIELDS)),
                    }
                }
                Ok(LeaveReason {
                    id: id.ok_or_else(|| de::Error::missing_field("id"))?,
                    count: count.ok_or_else(|| de::Error::missing_field("count"))?,
                    reason: reason.ok_or_else(|| de::Error::missing_field("reason"))?,
                })
            }
        }

        deserializer.deserialize_struct("LeaveReason", FIELDS, RecordVisitor)
    }
}

impl LeaveReason {
    /// A record with a fresh random ID and the sanitized `text`.
    pub fn new(text: &str, count: RestartCount) -> Result<Self, Error> {
        Self::with_id_source(text, count, getrandom::fill)
    }

    fn with_id_source(
        text: &str,
        count: RestartCount,
        fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
    ) -> Result<Self, Error> {
        let reason = ReasonText::sanitize(text).ok_or(Error::InvalidRecord(RecordError::Text))?;
        let mut bytes = [0_u8; 16];
        fill(&mut bytes).map_err(Error::Random)?;
        let id = bytes
            .iter()
            .flat_map(|byte| [byte >> 4, byte & 0xf])
            .map(|nibble| char::from(HEX[usize::from(nibble)]))
            .collect();
        Ok(Self {
            id: ReasonId(id),
            count,
            reason,
        })
    }

    /// Compact JSON of at most 300 printable ASCII bytes. The reason is cut where the whole
    /// record would not fit, counting two bytes for each `"` and `\` that JSON escapes.
    #[must_use]
    pub fn encode(&self) -> String {
        let budget = MAX_RECORD_BYTES.saturating_sub(RECORD_OVERHEAD + self.id.0.len());
        let mut used = 0;
        let mut end = 0;
        for byte in self.reason.0.bytes() {
            let cost = if matches!(byte, b'"' | b'\\') { 2 } else { 1 };
            if used + cost > budget {
                break;
            }
            used += cost;
            end += 1;
        }
        let reason = self.reason.0[..end].trim_end_matches(' ');
        // The ID is letters, digits, `.`, `_` and `-`, and the reason is printable ASCII, so only
        // `"` and `\` need a JSON escape.
        let mut record = String::with_capacity(MAX_RECORD_BYTES);
        record.push_str(r#"{"id":""#);
        record.push_str(&self.id.0);
        record.push_str(r#"","count":"#);
        record.push(char::from(b'0' + self.count.0));
        record.push_str(r#","reason":""#);
        for c in reason.chars() {
            if matches!(c, '"' | '\\') {
                record.push('\\');
            }
            record.push(c);
        }
        record.push_str(r#""}"#);
        record
    }

    pub fn decode(value: &str) -> Result<Self, Error> {
        value.parse()
    }
}

impl FromStr for LeaveReason {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Error> {
        if value.len() > MAX_RECORD_BYTES {
            return Err(Error::InvalidRecord(RecordError::TooLong));
        }
        if !value.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
            return Err(Error::InvalidRecord(RecordError::NotPrintable));
        }
        serde_json::from_str(value).map_err(|e| Error::InvalidRecord(RecordError::Syntax(e)))
    }
}

/// Whether the restart-limit hit grants another tuning boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartLimit {
    /// Count 1 or 2: the saved entry stays.
    Retry(RestartCount),
    Exhausted,
}

pub fn read_reason(env: &mut (impl Environment + ?Sized)) -> Result<Option<LeaveReason>, Error> {
    match env.get(REASON_VARIABLE).map_err(Error::ReadReason)? {
        Some(value) if !value.is_empty() => value.parse().map(Some),
        _ => Ok(None),
    }
}

pub fn write_reason(
    env: &mut (impl Environment + ?Sized),
    reason: &LeaveReason,
) -> Result<(), Error> {
    env.set(&[(REASON_VARIABLE, &reason.encode())])
        .map_err(Error::WriteReason)
}

pub fn clear_reason(env: &mut (impl Environment + ?Sized)) -> Result<(), Error> {
    env.unset(&[REASON_VARIABLE]).map_err(Error::ClearReason)
}

pub fn reset_count(env: &mut (impl Environment + ?Sized)) -> Result<(), Error> {
    env.unset(&[COUNT_VARIABLE]).map_err(Error::ResetCount)
}

/// Records the attempt, then reports whether a retry boot is granted. Count and reason are
/// written together before the saved entry is cleared on exhaustion, and any failure returns an
/// error instead of a retry.
pub fn restart_limit(bootloader: &mut (impl Bootloader + ?Sized)) -> Result<RestartLimit, Error> {
    restart_limit_with(bootloader, getrandom::fill)
}

/// `restart_limit` with the source of the record ID bytes passed in, so a test can fail it.
fn restart_limit_with(
    bootloader: &mut (impl Bootloader + ?Sized),
    fill: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
) -> Result<RestartLimit, Error> {
    let stored = bootloader.get(COUNT_VARIABLE).map_err(Error::ReadCount)?;
    let count = match stored.as_deref() {
        None | Some("" | "0") => 0,
        Some("1") => 1,
        Some("2") => 2,
        Some("3") => 3,
        Some(other) => return Err(Error::InvalidCount(other.to_owned())),
    };
    let count = RestartCount(count).next();
    let limit = if count.get() < MAX_COUNT {
        RestartLimit::Retry(count)
    } else {
        RestartLimit::Exhausted
    };
    let text = match limit {
        RestartLimit::Retry(_) => RETRY_REASON,
        RestartLimit::Exhausted => EXHAUSTED_REASON,
    };
    let record = LeaveReason::with_id_source(text, count, fill)?;
    bootloader
        .set(&[
            (COUNT_VARIABLE, &count.get().to_string()),
            (REASON_VARIABLE, &record.encode()),
        ])
        .map_err(Error::WriteCountAndReason)?;
    if limit == RestartLimit::Exhausted {
        bootloader
            .clear_saved_entry()
            .map_err(Error::ClearSavedEntry)?;
    }
    Ok(limit)
}

#[cfg(test)]
mod tests;
