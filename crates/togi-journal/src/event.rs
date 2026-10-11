use std::fmt;
use std::num::NonZeroU64;
use std::time::Duration;

use jiff::Timestamp;
use serde::de::Error as _;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use togi_machine::BootId;

use crate::payload::{EventKind, Payload, UnknownKind};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(NonZeroU64);

impl Seq {
    pub const FIRST: Self = Self(NonZeroU64::MIN);

    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }

    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stamp {
    pub time: Timestamp,
    pub mono: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: Seq,
    pub time: Timestamp,
    pub boot: BootId,
    pub mono: Option<Duration>,
    pub cause: Vec<Seq>,
    pub msg: String,
    pub payload: Payload,
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("malformed event: {0}")]
    Envelope(#[source] serde_json::Error),
    #[error("{0}")]
    UnknownKind(#[source] UnknownKind),
    #[error("{kind} event: {source}")]
    Payload {
        kind: EventKind,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    seq: Seq,
    time: Timestamp,
    boot: BootId,
    #[serde(default)]
    mono_ms: Option<u64>,
    #[serde(default)]
    cause: Vec<Seq>,
    kind: String,
    msg: String,
    data: Box<RawValue>,
}

impl Envelope {
    fn into_event(self) -> Result<Event, DecodeError> {
        let kind: EventKind = self.kind.parse().map_err(DecodeError::UnknownKind)?;
        let payload = Payload::decode(kind, self.data.get())
            .map_err(|source| DecodeError::Payload { kind, source })?;
        Ok(Event {
            seq: self.seq,
            time: self.time,
            boot: self.boot,
            mono: self.mono_ms.map(Duration::from_millis),
            cause: self.cause,
            msg: self.msg,
            payload,
        })
    }
}

impl Event {
    pub fn new(seq: Seq, stamp: Stamp, boot: BootId, cause: Vec<Seq>, payload: Payload) -> Self {
        Self {
            seq,
            time: stamp.time,
            boot,
            mono: stamp.mono,
            cause,
            msg: payload.message(),
            payload,
        }
    }

    pub const fn kind(&self) -> EventKind {
        self.payload.kind()
    }

    pub fn decode(line: &str) -> Result<Self, DecodeError> {
        serde_json::from_str::<Envelope>(line)
            .map_err(DecodeError::Envelope)?
            .into_event()
    }

    pub fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

impl Serialize for Event {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mono_ms = self
            .mono
            .map(|mono| u64::try_from(mono.as_millis()))
            .transpose()
            .map_err(serde::ser::Error::custom)?;
        let fields = 5 + usize::from(mono_ms.is_some()) + usize::from(!self.cause.is_empty());
        let mut event = serializer.serialize_struct("Event", fields)?;
        event.serialize_field("seq", &self.seq)?;
        event.serialize_field("time", &self.time)?;
        event.serialize_field("boot", &self.boot)?;
        if let Some(mono_ms) = &mono_ms {
            event.serialize_field("mono_ms", mono_ms)?;
        }
        if !self.cause.is_empty() {
            event.serialize_field("cause", &self.cause)?;
        }
        event.serialize_field("kind", self.kind().as_str())?;
        event.serialize_field("msg", &self.msg)?;
        event.serialize_field("data", &self.payload)?;
        event.end()
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Envelope::deserialize(deserializer)?
            .into_event()
            .map_err(D::Error::custom)
    }
}
