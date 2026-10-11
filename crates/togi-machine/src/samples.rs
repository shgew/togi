use crate::PerCore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use togi_trialfiles::TrialFile;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TrialConditions {
    pub elapsed_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tctl_c: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tccd_c: Option<BTreeMap<String, i64>>,
    #[serde(default, skip_serializing_if = "PerCore::is_empty")]
    pub core_mhz: PerCore<i64>,
    #[serde(default, skip_serializing_if = "PerCore::is_empty")]
    pub worker_cpu_ms: PerCore<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_power_w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pm_table: Option<PmTable>,
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
            if let Ok(sample) = serde_json::from_slice(&self.line) {
                return Some(sample);
            }
        }
    }
}
