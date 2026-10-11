use crate::PerCore;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use togi_trialfiles::TrialFile;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PmTable {
    pub power_w: [f32; 16],
    pub voltage_request_v: [f32; 16],
    pub temperature_c: [f32; 16],
    pub c0_pct: [f32; 16],
    pub cc1_pct: [f32; 16],
    pub cc6_pct: [f32; 16],
}

pub trait PmTableReader {
    fn pm_table(&self) -> Option<PmTable>;
}

struct Float32Array([f32; 16]);
impl<'de> Deserialize<'de> for Float32Array {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Array;
        impl<'de> Visitor<'de> for Array {
            type Value = Float32Array;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a float32 array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> Result<Self::Value, A::Error> {
                let mut values = [0.0; 16];
                let mut index = 0;
                while let Some(raw) = array.next_element::<Box<RawValue>>()? {
                    if index < values.len() && raw.get() != "null" {
                        let value: f32 = raw.get().parse().map_err(serde::de::Error::custom)?;
                        if !value.is_finite() {
                            return Err(serde::de::Error::custom("float32 overflow"));
                        }
                        values[index] = value;
                    }
                    index += 1;
                }
                Ok(Float32Array(values))
            }
        }
        deserializer.deserialize_seq(Array)
    }
}

struct TableUpdate<'a>(&'a mut PmTable);
impl<'de> serde::de::DeserializeSeed<'de> for TableUpdate<'_> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for TableUpdate<'_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a PM-table object or null")
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some((key, raw)) = map.next_entry::<String, Box<RawValue>>()? {
            let target = if field_eq(&key, "power_w") {
                &mut self.0.power_w
            } else if field_eq(&key, "voltage_request_v") {
                &mut self.0.voltage_request_v
            } else if field_eq(&key, "temperature_c") {
                &mut self.0.temperature_c
            } else if field_eq(&key, "c0_pct") {
                &mut self.0.c0_pct
            } else if field_eq(&key, "cc1_pct") {
                &mut self.0.cc1_pct
            } else if field_eq(&key, "cc6_pct") {
                &mut self.0.cc6_pct
            } else {
                continue;
            };
            if raw.get() != "null" {
                *target = serde_json::from_str::<Float32Array>(raw.get())
                    .map_err(serde::de::Error::custom)?
                    .0;
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for PmTable {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::DeserializeSeed;
        let mut table = Self::default();
        TableUpdate(&mut table).deserialize(deserializer)?;
        Ok(table)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TrialConditions {
    pub elapsed_ms: i64,
    pub tctl_c: Option<i64>,
    pub tccd_c: Option<BTreeMap<String, i64>>,
    pub core_mhz: PerCore<i64>,
    pub worker_cpu_ms: PerCore<i64>,
    pub package_power_w: Option<f64>,
    pub pm_table: Option<PmTable>,
}

struct Temperatures(BTreeMap<String, i64>);
impl<'de> Deserialize<'de> for Temperatures {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Readings;
        impl<'de> Visitor<'de> for Readings {
            type Value = Temperatures;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a temperature object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, raw)) = map.next_entry::<String, Box<RawValue>>()? {
                    let value = if raw.get() == "null" {
                        0
                    } else {
                        serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?
                    };
                    values.insert(key, value);
                }
                Ok(Temperatures(values))
            }
        }
        deserializer.deserialize_map(Readings)
    }
}

impl<'de> Deserialize<'de> for TrialConditions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Conditions;
        impl<'de> Visitor<'de> for Conditions {
            type Value = TrialConditions;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a conditions object or null")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(TrialConditions::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                use serde::de::DeserializeSeed;
                let mut sample = TrialConditions::default();
                while let Some((key, raw)) = map.next_entry::<String, Box<RawValue>>()? {
                    let raw = raw.get();
                    if field_eq(&key, "elapsed_ms") {
                        if raw != "null" {
                            sample.elapsed_ms =
                                serde_json::from_str(raw).map_err(serde::de::Error::custom)?;
                        }
                    } else if field_eq(&key, "tctl_c") {
                        sample.tctl_c =
                            serde_json::from_str(raw).map_err(serde::de::Error::custom)?;
                    } else if field_eq(&key, "tccd_c") {
                        if raw == "null" {
                            sample.tccd_c = None;
                        } else {
                            let values = serde_json::from_str::<Temperatures>(raw)
                                .map_err(serde::de::Error::custom)?
                                .0;
                            sample
                                .tccd_c
                                .get_or_insert_with(BTreeMap::new)
                                .extend(values);
                        }
                    } else if field_eq(&key, "core_mhz") {
                        sample.core_mhz =
                            serde_json::from_str(raw).map_err(serde::de::Error::custom)?;
                    } else if field_eq(&key, "worker_cpu_ms") {
                        sample.worker_cpu_ms =
                            serde_json::from_str(raw).map_err(serde::de::Error::custom)?;
                    } else if field_eq(&key, "package_power_w") {
                        sample.package_power_w =
                            serde_json::from_str(raw).map_err(serde::de::Error::custom)?;
                    } else if field_eq(&key, "pm_table") {
                        if raw == "null" {
                            sample.pm_table = None;
                        } else {
                            let mut decoder = serde_json::Deserializer::from_str(raw);
                            TableUpdate(sample.pm_table.get_or_insert_with(PmTable::default))
                                .deserialize(&mut decoder)
                                .map_err(serde::de::Error::custom)?;
                        }
                    }
                }
                Ok(sample)
            }
        }
        deserializer.deserialize_any(Conditions)
    }
}

pub struct ReadSamples<R: Read = TrialFile> {
    reader: Option<BufReader<R>>,
    line: Vec<u8>,
}

impl<R: Read> ReadSamples<R> {
    pub fn from_reader(reader: R) -> Self {
        Self {
            reader: Some(BufReader::new(reader)),
            line: Vec::new(),
        }
    }
}

pub fn read_samples(dir: &Path) -> ReadSamples {
    ReadSamples {
        reader: togi_trialfiles::open(&dir.join(togi_trialfiles::SAMPLES))
            .ok()
            .map(BufReader::new),
        line: Vec::new(),
    }
}

impl<R: Read> Iterator for ReadSamples<R> {
    type Item = TrialConditions;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            self.line.clear();
            match self.reader.as_mut()?.read_until(b'\n', &mut self.line) {
                Ok(length) if length > 0 && self.line.last() == Some(&b'\n') => {}
                Ok(_) | Err(_) => {
                    self.reader = None;
                    return None;
                }
            }
            let text = json_text(&self.line);
            if let Ok(sample) = serde_json::from_str(&text) {
                return Some(sample);
            }
        }
    }
}

fn json_text(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => Cow::Owned(crate::HostText::new(bytes.to_vec()).normalized()),
    };
    let mut result = String::new();
    let mut copied = 0;
    let mut index = 0;
    let mut string = false;
    while index < text.len() {
        let byte = text.as_bytes()[index];
        if byte == b'"' {
            string = !string;
            index += 1;
            continue;
        }
        if !string || byte != b'\\' {
            index += 1;
            continue;
        }
        if text.as_bytes().get(index + 1) != Some(&b'u') {
            index += 2;
            continue;
        }
        let Some(code) = hex_escape(&text, index) else {
            index += 2;
            continue;
        };
        if (0xd800..=0xdbff).contains(&code) {
            if let Some(next) = hex_escape(&text, index + 6) {
                if (0xdc00..=0xdfff).contains(&next) {
                    index += 12;
                    continue;
                }
            }
        } else if !(0xdc00..=0xdfff).contains(&code) {
            index += 6;
            continue;
        }
        result.push_str(&text[copied..index]);
        result.push_str("\\ufffd");
        index += 6;
        copied = index;
    }
    if copied == 0 {
        text
    } else {
        result.push_str(&text[copied..]);
        Cow::Owned(result)
    }
}

fn hex_escape(text: &str, index: usize) -> Option<u16> {
    if text.as_bytes().get(index..index + 2)? != b"\\u" {
        return None;
    }
    u16::from_str_radix(text.get(index + 2..index + 6)?, 16).ok()
}

fn field_eq(actual: &str, expected: &str) -> bool {
    actual
        .chars()
        .map(|ch| match ch {
            '\u{212a}' => 'k',
            '\u{17f}' => 's',
            ch => ch.to_ascii_lowercase(),
        })
        .eq(expected.chars())
}
