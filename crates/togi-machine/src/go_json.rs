//! Go-compatible JSON text: one string writer, one input normalizer and the
//! struct-decoding helpers every type in this crate shares.

use serde::de::{DeserializeOwned, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::fmt;

/// Go's `encoding/json` rejects documents nested deeper than this.
const MAX_DEPTH: usize = 10_000;

/// A document nests arrays or objects deeper than Go's `encoding/json` allows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("exceeded max depth")]
pub struct ExceededMaxDepth;

/// Normalizes Go-written JSON bytes into text that serde decodes as Go does.
///
/// Go's `json.Unmarshal` turns every invalid UTF-8 byte inside a string into
/// U+FFFD and every unpaired surrogate escape (`\ud800`) into U+FFFD, and
/// rejects nesting deeper than 10000 levels anywhere in the document, ignored
/// values included. Serde rejects the first two and accepts any depth, so
/// every decode of bytes Go wrote goes through this function first:
/// `TrialConditions`, `PmTable`, the vocabulary types, `HostText`, `CoreInfo`,
/// `BiosContext` and `PstoreRecord`.
///
/// The typed `Deserialize` implementations see text that is already UTF-8 and
/// do not count depth, so on their own they accept a different language than
/// Go; call this first. `ReadSamples` does.
pub fn json_text(bytes: &[u8]) -> Result<Cow<'_, str>, ExceededMaxDepth> {
    let text = if let Ok(text) = std::str::from_utf8(bytes) {
        Cow::Borrowed(text)
    } else {
        let mut lossy = String::with_capacity(bytes.len());
        for_each_char(bytes, |ch| lossy.push(ch));
        Cow::Owned(lossy)
    };
    let raw = text.as_bytes();
    let mut result = String::new();
    let mut copied = 0;
    let mut index = 0;
    let mut in_string = false;
    let mut depth = 0_usize;
    while index < raw.len() {
        match raw[index] {
            b'"' => {
                in_string = !in_string;
                index += 1;
            }
            b'[' | b'{' if !in_string => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(ExceededMaxDepth);
                }
                index += 1;
            }
            b']' | b'}' if !in_string => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            b'\\' if in_string => {
                let Some(code) = hex_escape(&text, index) else {
                    index += 2;
                    continue;
                };
                let paired = (0xd800..=0xdbff).contains(&code)
                    && hex_escape(&text, index + 6)
                        .is_some_and(|next| (0xdc00..=0xdfff).contains(&next));
                if paired {
                    index += 12;
                } else if (0xd800..=0xdfff).contains(&code) {
                    result.push_str(&text[copied..index]);
                    result.push_str("\\ufffd");
                    index += 6;
                    copied = index;
                } else {
                    index += 6;
                }
            }
            _ => index += 1,
        }
    }
    if copied == 0 {
        Ok(text)
    } else {
        result.push_str(&text[copied..]);
        Ok(Cow::Owned(result))
    }
}

fn hex_escape(text: &str, index: usize) -> Option<u16> {
    if text.as_bytes().get(index..index + 2)? != b"\\u" {
        return None;
    }
    u16::from_str_radix(text.get(index + 2..index + 6)?, 16).ok()
}

/// Visits the characters of `bytes` as Go converts them: each invalid byte is
/// one U+FFFD.
pub(crate) fn for_each_char(bytes: &[u8], mut visit: impl FnMut(char)) {
    let mut remaining = bytes;
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(text) => {
                text.chars().for_each(&mut visit);
                return;
            }
            Err(error) => {
                if let Ok(text) = std::str::from_utf8(&remaining[..error.valid_up_to()]) {
                    text.chars().for_each(&mut visit);
                }
                visit('\u{fffd}');
                remaining = &remaining[error.valid_up_to() + 1..];
            }
        }
    }
}

/// Appends `bytes` as a JSON string the way Go's `encoding/json` writes it:
/// invalid bytes become the literal U+FFFD, one each; `\b` and `\f` keep their
/// short escapes; U+2028 and U+2029 are escaped; `<`, `>` and `&` are escaped
/// only when `escape_html` (`json.Marshal`; the journal writer turns it off).
pub(crate) fn write_json_string(out: &mut String, bytes: &[u8], escape_html: bool) {
    use std::fmt::Write;
    out.push('"');
    for_each_char(bytes, |ch| match ch {
        '"' => out.push_str("\\\""),
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{8}' => out.push_str("\\b"),
        '\u{c}' => out.push_str("\\f"),
        '\u{2028}' => out.push_str("\\u2028"),
        '\u{2029}' => out.push_str("\\u2029"),
        '<' | '>' | '&' if escape_html => {
            let _ = write!(out, "\\u{:04x}", u32::from(ch));
        }
        ch if ch < '\u{20}' => {
            let _ = write!(out, "\\u{:04x}", u32::from(ch));
        }
        ch => out.push(ch),
    });
    out.push('"');
}

/// Whether `serde_json` writes `text` as the same JSON string as
/// `write_json_string`: for UTF-8 text the two escape quotes, backslashes and
/// control characters alike, and differ only on U+2028, U+2029 and, with
/// `escape_html`, `<`, `>` and `&`.
pub(crate) fn serde_json_writes_as_go(text: &str, escape_html: bool) -> bool {
    !text.chars().any(|ch| match ch {
        '\u{2028}' | '\u{2029}' => true,
        '<' | '>' | '&' => escape_html,
        _ => false,
    })
}

/// Whether the JSON object key `actual` names the field `expected` under Go's
/// case folding (which maps U+212A to `k` and U+017F to `s`).
pub(crate) fn field_eq(actual: &str, expected: &str) -> bool {
    actual
        .chars()
        .map(|ch| match ch {
            '\u{212a}' => 'k',
            '\u{17f}' => 's',
            ch => ch.to_ascii_lowercase(),
        })
        .eq(expected.chars())
}

/// Decodes one JSON text and requires nothing after it.
pub(crate) fn parse<T: DeserializeOwned>(text: &str) -> Result<T, serde_json::Error> {
    let mut decoder = serde_json::Deserializer::from_str(text);
    let value = T::deserialize(&mut decoder)?;
    decoder.end()?;
    Ok(value)
}

/// Go's decoding of a non-pointer scalar: JSON `null` leaves `slot` alone.
pub(crate) fn scalar<T: DeserializeOwned>(
    slot: &mut T,
    raw: &str,
) -> Result<(), serde_json::Error> {
    if raw != "null" {
        *slot = parse(raw)?;
    }
    Ok(())
}

/// Go's decoding of a struct: a JSON object updates `state` one member at a
/// time in document order (so duplicates are successive updates), member names
/// are matched by the caller with `field_eq`, unknown members are skipped and
/// JSON `null` changes nothing. Any other JSON value is an error.
pub(crate) fn decode_fields<'de, D, S, F>(
    deserializer: D,
    state: &mut S,
    expecting: &'static str,
    apply: F,
) -> Result<(), D::Error>
where
    D: Deserializer<'de>,
    F: FnMut(&mut S, &str, &str) -> Result<(), serde_json::Error>,
{
    struct Object<'a, S, F> {
        state: &'a mut S,
        apply: F,
        expecting: &'static str,
    }
    impl<'de, S, F> Visitor<'de> for Object<'_, S, F>
    where
        F: FnMut(&mut S, &str, &str) -> Result<(), serde_json::Error>,
    {
        type Value = ();
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.expecting)
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
            Ok(())
        }
        fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<(), A::Error> {
            while let Some((key, raw)) = map.next_entry::<String, Box<RawValue>>()? {
                (self.apply)(self.state, &key, raw.get()).map_err(serde::de::Error::custom)?;
            }
            Ok(())
        }
    }
    deserializer.deserialize_any(Object {
        state,
        apply,
        expecting,
    })
}

/// `decode_fields` over a JSON text that an enclosing object already extracted.
pub(crate) fn update_object<S>(
    raw: &str,
    state: &mut S,
    expecting: &'static str,
    apply: impl FnMut(&mut S, &str, &str) -> Result<(), serde_json::Error>,
) -> Result<(), serde_json::Error> {
    let mut decoder = serde_json::Deserializer::from_str(raw);
    decode_fields(&mut decoder, state, expecting, apply)?;
    decoder.end()
}

/// A Go slice field under successive updates. Go decodes an array into the
/// slice's existing backing store, so a `null` element keeps the value an
/// earlier array left at that index; `null` and `[]` start over.
#[derive(Debug)]
pub(crate) struct GoSlice<T> {
    elements: Vec<T>,
    len: Option<usize>,
}

impl<T> Default for GoSlice<T> {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            len: None,
        }
    }
}

impl<T: Default + DeserializeOwned> GoSlice<T> {
    pub(crate) fn update(&mut self, raw: &str) -> Result<(), serde_json::Error> {
        if raw == "null" {
            self.elements.clear();
            self.len = None;
            return Ok(());
        }
        let items: Vec<Box<RawValue>> = parse(raw)?;
        if items.is_empty() {
            self.elements.clear();
        }
        for (index, item) in items.iter().enumerate() {
            if index == self.elements.len() {
                self.elements.push(T::default());
            }
            scalar(&mut self.elements[index], item.get())?;
        }
        self.len = Some(items.len());
        Ok(())
    }

    pub(crate) fn into_option(mut self) -> Option<Vec<T>> {
        let len = self.len?;
        self.elements.truncate(len);
        Some(self.elements)
    }
}
