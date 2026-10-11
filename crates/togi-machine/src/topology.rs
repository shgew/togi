use crate::{CcdId, CoreId, CpuId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CoreInfo {
    pub core: CoreId,
    pub ccd: CcdId,
    pub cpus: Option<Vec<CpuId>>,
}

pub fn order(cores: &[CoreInfo]) -> Vec<CoreId> {
    let mut sorted: Vec<_> = cores.iter().collect();
    sorted.sort_by_key(|core| (core.ccd, core.core));
    let mut result = Vec::with_capacity(cores.len());
    for index in 0..cores.len() {
        let before = result.len();
        for ccd in sorted.chunk_by(|a, b| a.ccd == b.ccd) {
            if let Some(core) = ccd.get(index) {
                result.push(core.core);
            }
        }
        if result.len() == before {
            break;
        }
    }
    result
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostText(Vec<u8>);

impl HostText {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn normalized(&self) -> String {
        let mut result = String::new();
        self.characters(|ch, _| result.push(ch));
        result
    }

    fn characters(&self, mut visit: impl FnMut(char, bool)) {
        let mut remaining = self.0.as_slice();
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(text) => {
                    for ch in text.chars() {
                        visit(ch, false);
                    }
                    break;
                }
                Err(error) => {
                    if let Ok(text) = std::str::from_utf8(&remaining[..error.valid_up_to()]) {
                        for ch in text.chars() {
                            visit(ch, false);
                        }
                    }
                    visit('\u{fffd}', true);
                    remaining = &remaining[error.valid_up_to() + 1..];
                }
            }
        }
    }

    pub fn json(&self, escape_html: bool) -> String {
        use std::fmt::Write;
        let mut result = String::from("\"");
        self.characters(|ch, invalid| match ch {
            _ if invalid => result.push_str("\\ufffd"),
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            '\u{8}' => result.push_str("\\b"),
            '\u{c}' => result.push_str("\\f"),
            '\u{2028}' => result.push_str("\\u2028"),
            '\u{2029}' => result.push_str("\\u2029"),
            '<' | '>' | '&' if escape_html => {
                let _ = write!(result, "\\u{:04x}", u32::from(ch));
            }
            ch if ch < '\u{20}' => {
                let _ = write!(result, "\\u{:04x}", u32::from(ch));
            }
            ch => result.push(ch),
        });
        result.push('"');
        result
    }
}

impl From<&str> for HostText {
    fn from(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }
}

impl Serialize for HostText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let raw = serde_json::value::RawValue::from_string(self.json(true))
            .map_err(serde::ser::Error::custom)?;
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HostText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::new(String::deserialize(deserializer)?.into_bytes()))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BiosContext {
    pub bios_version: HostText,
    pub board: HostText,
    pub cpu_model: HostText,
    pub microcode: HostText,
    pub boost_limit_mhz: i64,
}

pub fn compare_context(recorded: &BiosContext, current: &BiosContext) -> (String, bool) {
    for (name, before, after) in [
        (
            "bios_version",
            &recorded.bios_version,
            &current.bios_version,
        ),
        ("board", &recorded.board, &current.board),
        ("cpu_model", &recorded.cpu_model, &current.cpu_model),
        ("microcode", &recorded.microcode, &current.microcode),
    ] {
        let normalized = after.normalized();
        if before.bytes() != normalized.as_bytes() {
            return (
                format!(
                    "{name} is {normalized}; the session recorded {}",
                    before.normalized()
                ),
                false,
            );
        }
    }
    if recorded.boost_limit_mhz != current.boost_limit_mhz {
        return (
            format!(
                "boost_limit_mhz is {}; the session recorded {}",
                current.boost_limit_mhz, recorded.boost_limit_mhz
            ),
            false,
        );
    }
    ("matches the session".to_owned(), true)
}
