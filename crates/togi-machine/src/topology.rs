use crate::go_json::{GoSlice, decode_fields, field_eq, for_each_char, scalar, write_json_string};
use crate::{CcdId, CoreId, CpuId, PstoreRecord};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct CoreInfo {
    pub core: CoreId,
    pub ccd: CcdId,
    pub cpus: Option<Vec<CpuId>>,
}

/// Decodes as Go's `json.Unmarshal` does into `CoreInfo`: names match under
/// Go's case folding, later duplicates update earlier ones, `null` leaves a
/// number alone and empties `cpus`, unknown members are skipped. `core` stays
/// checked to 0–15. Bytes Go wrote go through `json_text` first.
impl<'de> Deserialize<'de> for CoreInfo {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut info = Self::default();
        let mut cpus = GoSlice::<CpuId>::default();
        decode_fields(
            deserializer,
            &mut (&mut info, &mut cpus),
            "a core object or null",
            |(info, cpus), key, raw| {
                if field_eq(key, "core") {
                    scalar(&mut info.core, raw)
                } else if field_eq(key, "ccd") {
                    scalar(&mut info.ccd, raw)
                } else if field_eq(key, "cpus") {
                    cpus.update(raw)
                } else {
                    Ok(())
                }
            },
        )?;
        info.cpus = cpus.into_option();
        Ok(info)
    }
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

/// Bytes a host reported, which need not be UTF-8.
///
/// Go carries them in a `string` and converts every invalid byte to U+FFFD
/// when it writes or compares them; `normalized`, `json` and `Serialize` do
/// the same, so a text that went through JSON compares equal to its
/// normalization. The literal U+FFFD is the only form of a replacement.
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
        for_each_char(&self.0, |ch| result.push(ch));
        result
    }

    /// The JSON string Go writes: `json.Marshal` with `escape_html`, the
    /// journal writer without it.
    pub fn json(&self, escape_html: bool) -> String {
        let mut result = String::new();
        write_json_string(&mut result, &self.0, escape_html);
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

/// Decodes a string, or `null` as empty. Bytes Go wrote go through
/// `json_text` first.
impl<'de> Deserialize<'de> for HostText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::new(
            Option::<String>::deserialize(deserializer)?
                .unwrap_or_default()
                .into_bytes(),
        ))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct BiosContext {
    pub bios_version: HostText,
    pub board: HostText,
    pub cpu_model: HostText,
    pub microcode: HostText,
    pub boost_limit_mhz: i64,
}

/// Decodes as Go's `json.Unmarshal` does into `BIOSContext`. Bytes Go wrote
/// go through `json_text` first.
impl<'de> Deserialize<'de> for BiosContext {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut context = Self::default();
        decode_fields(
            deserializer,
            &mut context,
            "a BIOS context object or null",
            |context, key, raw| {
                if field_eq(key, "bios_version") {
                    scalar(&mut context.bios_version, raw)
                } else if field_eq(key, "board") {
                    scalar(&mut context.board, raw)
                } else if field_eq(key, "cpu_model") {
                    scalar(&mut context.cpu_model, raw)
                } else if field_eq(key, "microcode") {
                    scalar(&mut context.microcode, raw)
                } else if field_eq(key, "boost_limit_mhz") {
                    scalar(&mut context.boost_limit_mhz, raw)
                } else {
                    Ok(())
                }
            },
        )?;
        Ok(context)
    }
}

/// Decodes as Go's `json.Unmarshal` does into `PstoreRecord`. Bytes Go wrote
/// go through `json_text` first.
impl<'de> Deserialize<'de> for PstoreRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut path = HostText::default();
        let mut lines = GoSlice::<HostText>::default();
        decode_fields(
            deserializer,
            &mut (&mut path, &mut lines),
            "a pstore record object or null",
            |(path, lines), key, raw| {
                if field_eq(key, "path") {
                    scalar(&mut **path, raw)
                } else if field_eq(key, "lines") {
                    lines.update(raw)
                } else {
                    Ok(())
                }
            },
        )?;
        Ok(Self {
            path,
            lines: lines.into_option(),
        })
    }
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
