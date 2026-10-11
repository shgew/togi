use crate::PerCore;
use crate::go_json::{decode_fields, field_eq, json_text, parse, scalar, update_object};
use serde::de::{MapAccess, Visitor};
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

/// Go's decoding of a `[16]float32` field: `null` changes nothing, a JSON
/// array writes into the existing elements (a `null` element keeps its value),
/// zeroes the positions the array does not reach and ignores extra elements.
fn update_floats(slot: &mut [f32; 16], raw: &str) -> Result<(), serde_json::Error> {
    if raw == "null" {
        return Ok(());
    }
    let items: Vec<Box<RawValue>> = parse(raw)?;
    for (index, value) in slot.iter_mut().enumerate() {
        match items.get(index).map(|item| item.get()) {
            None => *value = 0.0,
            Some("null") => {}
            Some(item) => {
                let parsed: f32 = item.parse().map_err(serde::de::Error::custom)?;
                if !parsed.is_finite() {
                    return Err(serde::de::Error::custom("float32 overflow"));
                }
                *value = parsed;
            }
        }
    }
    Ok(())
}

fn update_table_member(table: &mut PmTable, key: &str, raw: &str) -> Result<(), serde_json::Error> {
    let slot = if field_eq(key, "power_w") {
        &mut table.power_w
    } else if field_eq(key, "voltage_request_v") {
        &mut table.voltage_request_v
    } else if field_eq(key, "temperature_c") {
        &mut table.temperature_c
    } else if field_eq(key, "c0_pct") {
        &mut table.c0_pct
    } else if field_eq(key, "cc1_pct") {
        &mut table.cc1_pct
    } else if field_eq(key, "cc6_pct") {
        &mut table.cc6_pct
    } else {
        return Ok(());
    };
    update_floats(slot, raw)
}

const PM_TABLE: &str = "a PM-table object or null";

/// Decodes as Go's `json.Unmarshal` does into `PMTable`. Bytes Go wrote go
/// through `json_text` first.
impl<'de> Deserialize<'de> for PmTable {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut table = Self::default();
        decode_fields(deserializer, &mut table, PM_TABLE, update_table_member)?;
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
                    let mut value = 0;
                    scalar(&mut value, raw.get()).map_err(serde::de::Error::custom)?;
                    values.insert(key, value);
                }
                Ok(Temperatures(values))
            }
        }
        deserializer.deserialize_map(Readings)
    }
}

fn update_condition(
    sample: &mut TrialConditions,
    key: &str,
    raw: &str,
) -> Result<(), serde_json::Error> {
    if field_eq(key, "elapsed_ms") {
        scalar(&mut sample.elapsed_ms, raw)
    } else if field_eq(key, "tctl_c") {
        sample.tctl_c = parse(raw)?;
        Ok(())
    } else if field_eq(key, "tccd_c") {
        if raw == "null" {
            sample.tccd_c = None;
        } else {
            let values = parse::<Temperatures>(raw)?.0;
            sample
                .tccd_c
                .get_or_insert_with(BTreeMap::new)
                .extend(values);
        }
        Ok(())
    } else if field_eq(key, "core_mhz") {
        sample.core_mhz = parse(raw)?;
        Ok(())
    } else if field_eq(key, "worker_cpu_ms") {
        sample.worker_cpu_ms = parse(raw)?;
        Ok(())
    } else if field_eq(key, "package_power_w") {
        sample.package_power_w = parse(raw)?;
        Ok(())
    } else if field_eq(key, "pm_table") {
        if raw == "null" {
            sample.pm_table = None;
            Ok(())
        } else {
            update_object(
                raw,
                sample.pm_table.get_or_insert_with(PmTable::default),
                PM_TABLE,
                update_table_member,
            )
        }
    } else {
        Ok(())
    }
}

/// Decodes as Go's `json.Unmarshal` does into `TrialConditions`. Bytes Go
/// wrote go through `json_text` first; `ReadSamples` does that.
impl<'de> Deserialize<'de> for TrialConditions {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut sample = Self::default();
        decode_fields(
            deserializer,
            &mut sample,
            "a conditions object or null",
            update_condition,
        )?;
        Ok(sample)
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
            let Ok(text) = json_text(&self.line) else {
                continue;
            };
            if let Ok(sample) = parse(&text) {
                return Some(sample);
            }
        }
    }
}
