use crate::PerCore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};

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
    pub cores_power_w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pm_table: Option<PmTable>,
}

#[derive(Debug, thiserror::Error)]
pub enum SampleError {
    #[error("open conditions samples")]
    Open(#[source] std::io::Error),
    #[error("read conditions samples (including gzip decoding)")]
    Read(#[source] std::io::Error),
    #[error("decode conditions sample")]
    Decode(#[source] serde_json::Error),
    #[error("read conditions sample elapsed milliseconds")]
    ElapsedMillis(#[source] std::num::TryFromIntError),
    #[error("record conditions sample elapsed seconds")]
    ElapsedSeconds(#[source] std::num::TryFromIntError),
    #[error("record conditions sample temperature")]
    Temperature(#[source] std::num::TryFromIntError),
    #[error("record conditions sample clock")]
    Clock(#[source] std::num::TryFromIntError),
}

pub struct ReadSamples<R: Read> {
    reader: Option<BufReader<R>>,
    line: Vec<u8>,
    error: Option<SampleError>,
}

impl<R: Read> ReadSamples<R> {
    pub fn from_reader(reader: R) -> Self {
        Self {
            reader: Some(BufReader::new(reader)),
            line: Vec::new(),
            error: None,
        }
    }

    pub fn from_open(reader: std::io::Result<R>) -> Self {
        match reader {
            Ok(reader) => Self::from_reader(reader),
            Err(error) => Self {
                reader: None,
                line: Vec::new(),
                error: (error.kind() != std::io::ErrorKind::NotFound)
                    .then_some(SampleError::Open(error)),
            },
        }
    }
}

impl<R: Read> Iterator for ReadSamples<R> {
    type Item = Result<TrialConditions, SampleError>;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(error) = self.error.take() {
            return Some(Err(error));
        }
        self.line.clear();
        match self.reader.as_mut()?.read_until(b'\n', &mut self.line) {
            Ok(length) if length > 0 && self.line.last() == Some(&b'\n') => {}
            Ok(_) => {
                self.reader = None;
                return None;
            }
            Err(error) => {
                self.reader = None;
                return Some(Err(SampleError::Read(error)));
            }
        }
        match serde_json::from_slice(&self.line) {
            Ok(sample) => Some(Ok(sample)),
            Err(error) => {
                self.reader = None;
                Some(Err(SampleError::Decode(error)))
            }
        }
    }
}
