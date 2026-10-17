use std::borrow::Cow;
use std::fmt;
use std::num::NonZeroU64;
use std::time::Duration;

use jiff::Timestamp;
use serde::de::Error as _;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use togi_machine::{BootId, serde_object::Object};

use crate::payload::{EventKind, Payload, UnknownKind};
use crate::unknown::{self, Step, Unknown, is_unknown_field};

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
    #[error("malformed {}event", kind_prefix(*.kind))]
    Envelope {
        /// The record's `kind` when it names a known event kind.
        kind: Option<EventKind>,
        #[source]
        source: serde_json::Error,
    },
    #[error("decode event kind")]
    UnknownKind(#[source] UnknownKind),
    #[error("{kind} event: unknown field {field:?}")]
    UnknownField {
        kind: EventKind,
        /// Where the field sits in the line: `extra` in the envelope, `data.extra` in the payload.
        field: String,
    },
    #[error("{kind} event")]
    Payload {
        kind: EventKind,
        #[source]
        source: serde_json::Error,
    },
}

/// How a line is decoded. Strict refuses what this build does not know: an unknown kind or an unknown field.
/// Lenient reads past both and reports them; every other fault is an error in both.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Decoding {
    Strict,
    Lenient,
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

/// An event whose envelope is sound and whose kind this build does not know.
pub(crate) struct Unrecognized {
    pub(crate) seq: Seq,
    pub(crate) cause: Vec<Seq>,
    pub(crate) kind: String,
}

impl Unrecognized {
    fn into_error(self) -> DecodeError {
        DecodeError::UnknownKind(UnknownKind(self.kind))
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "a decoded event is moved out once; boxing it would allocate for every line read"
)]
pub(crate) enum Decoded {
    Event(Event),
    Unrecognized(Unrecognized),
}

/// A decoded line and the unknown fields that were cut out of it to decode it.
pub(crate) struct Read {
    pub(crate) decoded: Decoded,
    pub(crate) ignored: Vec<String>,
}

/// Why one decode attempt failed. An unknown field carries the error to report when it cannot be cut out.
enum Failure {
    Error(DecodeError),
    Unknown {
        unknown: Unknown,
        kind: Option<EventKind>,
        error: DecodeError,
    },
}

fn envelope_failure(line: &str, source: serde_json::Error) -> Failure {
    let kind = known_kind(line);
    let unknown = (is_unknown_field(&source))
        .then(|| {
            let tracked = serde_path_to_error::deserialize::<_, Object<Envelope>>(
                &mut serde_json::Deserializer::from_str(line),
            );
            tracked.err().and_then(|error| {
                unknown::locate(line, &[], &error, |text| {
                    serde_json::from_str::<Object<Envelope>>(text).map(drop)
                })
            })
        })
        .flatten();
    let error = DecodeError::Envelope { kind, source };
    match unknown {
        Some(unknown) => Failure::Unknown {
            unknown,
            kind,
            error,
        },
        None => Failure::Error(error),
    }
}

fn payload_failure(line: &str, kind: EventKind, data: &str, source: serde_json::Error) -> Failure {
    let unknown = is_unknown_field(&source)
        .then(|| {
            Payload::decode_tracked(kind, data).err().and_then(|error| {
                unknown::locate(line, &[Step::Key("data".to_owned())], &error, |text| {
                    Payload::decode(kind, text).map(drop)
                })
            })
        })
        .flatten();
    let error = DecodeError::Payload { kind, source };
    match unknown {
        Some(unknown) => Failure::Unknown {
            unknown,
            kind: Some(kind),
            error,
        },
        None => Failure::Error(error),
    }
}

fn decode_once(line: &str) -> Result<Decoded, Failure> {
    let envelope = match serde_json::from_str::<Object<Envelope>>(line) {
        Ok(Object(envelope)) => envelope,
        Err(source) => return Err(envelope_failure(line, source)),
    };
    let Ok(kind) = envelope.kind.parse::<EventKind>() else {
        return Ok(Decoded::Unrecognized(Unrecognized {
            seq: envelope.seq,
            cause: envelope.cause,
            kind: envelope.kind,
        }));
    };
    let payload = Payload::decode(kind, envelope.data.get())
        .map_err(|source| payload_failure(line, kind, envelope.data.get(), source))?;
    Ok(Decoded::Event(Event {
        seq: envelope.seq,
        time: envelope.time,
        boot: envelope.boot,
        mono: envelope.mono_ms.map(Duration::from_millis),
        cause: envelope.cause,
        msg: envelope.msg,
        payload,
    }))
}

/// Decodes one line; the one decode that strict and lenient reads share.
///
/// The line is decoded as written. Only when that meets an unknown field is the field cut out and the rest decoded
/// again, so the common line costs one pass. Strict refuses at the first unknown field of a line whose kind it knows.
/// Otherwise the loop ends once every unknown field is out, and strict then refuses what was cut or skipped.
pub(crate) fn decode(line: &str, decoding: Decoding) -> Result<Read, DecodeError> {
    let mut text = Cow::Borrowed(line);
    let mut ignored = Vec::new();
    let decoded = loop {
        match decode_once(&text) {
            Ok(decoded) => break decoded,
            Err(Failure::Error(error)) => return Err(error),
            Err(Failure::Unknown {
                unknown,
                kind,
                error,
            }) => {
                if let (Decoding::Strict, Some(kind)) = (decoding, kind) {
                    return Err(DecodeError::UnknownField {
                        kind,
                        field: unknown.path(),
                    });
                }
                let Some(stripped) = unknown.remove_from(&text) else {
                    return Err(error);
                };
                ignored.push(unknown.path());
                text = Cow::Owned(stripped);
            }
        }
    };
    if decoding == Decoding::Lenient {
        return Ok(Read { decoded, ignored });
    }
    match decoded {
        Decoded::Unrecognized(unrecognized) => Err(unrecognized.into_error()),
        Decoded::Event(event) => match ignored.into_iter().next() {
            Some(field) => Err(DecodeError::UnknownField {
                kind: event.kind(),
                field,
            }),
            None => Ok(Read {
                decoded: Decoded::Event(event),
                ignored: Vec::new(),
            }),
        },
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
        match decode(line, Decoding::Strict)?.decoded {
            Decoded::Event(event) => Ok(event),
            Decoded::Unrecognized(unrecognized) => Err(unrecognized.into_error()),
        }
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
        let record = Box::<RawValue>::deserialize(deserializer)?;
        Self::decode(record.get())
            .map_err(|error| D::Error::custom(togi_machine::error_chain(&error)))
    }
}

fn kind_prefix(kind: Option<EventKind>) -> String {
    kind.map_or_else(String::new, |kind| format!("{kind} "))
}

/// The `kind` of a record whose envelope failed strict decoding, if it names one.
fn known_kind(line: &str) -> Option<EventKind> {
    // Owned: a kind spelled with a JSON escape (`session\u002estart`) cannot be borrowed.
    #[derive(Deserialize)]
    struct Probe {
        kind: String,
    }
    serde_json::from_str::<Object<Probe>>(line)
        .ok()
        .and_then(|Object(probe)| probe.kind.parse().ok())
}
