use std::fmt;
use std::num::NonZeroU32;
use std::str::FromStr;

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const STAMP_LEN: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidSessionId {
    #[error("session id {0:?} is not YYYYMMDDTHHMMSSZ or YYYYMMDDTHHMMSSZ-N")]
    Malformed(String),
    #[error("session id suffix must be 2 or more, not {0}")]
    Suffix(u32),
    #[error("session time is outside the years 0000 to 9999")]
    Time,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId {
    time: Timestamp,
    suffix: Option<NonZeroU32>,
}

impl SessionId {
    pub fn new(time: Timestamp, suffix: Option<NonZeroU32>) -> Result<Self, InvalidSessionId> {
        if let Some(suffix) = suffix.filter(|suffix| suffix.get() < 2) {
            return Err(InvalidSessionId::Suffix(suffix.get()));
        }
        let civil = time.to_zoned(TimeZone::UTC).datetime();
        if !(0..=9999).contains(&civil.year()) {
            return Err(InvalidSessionId::Time);
        }
        let truncated = civil
            .with()
            .subsec_nanosecond(0)
            .build()
            .and_then(|civil| civil.to_zoned(TimeZone::UTC))
            .map_err(|_| InvalidSessionId::Time)?;
        Ok(Self {
            time: truncated.timestamp(),
            suffix,
        })
    }

    pub const fn timestamp(&self) -> Timestamp {
        self.time
    }

    pub const fn suffix(&self) -> Option<NonZeroU32> {
        self.suffix
    }

    pub fn next_suffix(&self) -> Option<Self> {
        let next = match self.suffix {
            None => NonZeroU32::MIN.checked_add(1)?,
            Some(suffix) => suffix.checked_add(1)?,
        };
        Some(Self {
            time: self.time,
            suffix: Some(next),
        })
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let civil = self.time.to_zoned(TimeZone::UTC).datetime();
        write!(
            f,
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            civil.year(),
            civil.month(),
            civil.day(),
            civil.hour(),
            civil.minute(),
            civil.second()
        )?;
        if let Some(suffix) = self.suffix {
            write!(f, "-{suffix}")?;
        }
        Ok(())
    }
}

impl FromStr for SessionId {
    type Err = InvalidSessionId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let malformed = || InvalidSessionId::Malformed(text.to_owned());
        let (stamp, suffix) = match text.split_once('-') {
            Some((stamp, suffix)) => (stamp, Some(suffix)),
            None => (text, None),
        };
        let bytes = stamp.as_bytes();
        let shape = bytes.len() == STAMP_LEN
            && bytes[8] == b'T'
            && bytes[15] == b'Z'
            && bytes[..8]
                .iter()
                .chain(&bytes[9..15])
                .all(u8::is_ascii_digit);
        if !shape {
            return Err(malformed());
        }
        let number =
            |range: std::ops::Range<usize>| stamp[range].parse::<i16>().map_err(|_| malformed());
        let civil = DateTime::new(
            number(0..4)?,
            number(4..6)?.try_into().map_err(|_| malformed())?,
            number(6..8)?.try_into().map_err(|_| malformed())?,
            number(9..11)?.try_into().map_err(|_| malformed())?,
            number(11..13)?.try_into().map_err(|_| malformed())?,
            number(13..15)?.try_into().map_err(|_| malformed())?,
            0,
        )
        .map_err(|_| malformed())?;
        let time = civil
            .to_zoned(TimeZone::UTC)
            .map_err(|_| malformed())?
            .timestamp();
        let suffix = match suffix {
            None => None,
            Some(digits) => {
                if digits.is_empty()
                    || !digits.bytes().all(|byte| byte.is_ascii_digit())
                    || digits.starts_with('0')
                {
                    return Err(malformed());
                }
                let value: u32 = digits.parse().map_err(|_| malformed())?;
                Some(NonZeroU32::new(value).ok_or_else(malformed)?)
            }
        };
        Self::new(time, suffix)
    }
}

impl Serialize for SessionId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}
