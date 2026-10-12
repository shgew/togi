//! Conditions samples of the last trial, generated lazily: one per simulated second, strictly before the
//! trial's end. Memory holds the snapshot that generates them, never the series.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Duration;
use togi_machine::{CoreId, CoreSet, PerCore, PmTable, TrialConditions};

use crate::voltage::VoltageState;

const TICK: Duration = Duration::from_secs(1);
/// A culprit's worker stops this long before the reset when the trial ran long enough.
const STALL_LEAD: Duration = Duration::from_secs(2);
const CCD_SIZE: usize = 8;

/// What the samples of one trial are generated from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LastTrial {
    pub id: Option<String>,
    pub cores: CoreSet,
    pub threads: u8,
    pub ran: Duration,
    /// The core whose worker stopped before the machine crashed.
    pub stalled: Option<CoreId>,
    pub voltage: Option<VoltageState>,
}

impl LastTrial {
    /// One sample per simulated second. The clocks and PM table never change within a trial, so every
    /// sample carries the same ones; worker CPU times advance with the tick.
    pub(crate) fn conditions(&self) -> Conditions {
        let mut core_mhz = PerCore::default();
        let pm_table = self.voltage.as_ref().map(|state| {
            let mut pm = PmTable {
                voltage_request_v: state.requests,
                ..PmTable::default()
            };
            for core in self.cores {
                let mhz = state.clocks[core.index() / CCD_SIZE];
                core_mhz.set(core, i64::from(mhz));
                pm.c0_pct[core.index()] = 100.0;
            }
            for (cc6, c0) in pm.cc6_pct.iter_mut().zip(pm.c0_pct) {
                *cc6 = 100.0 - c0;
            }
            pm
        });
        Conditions {
            trial: self.clone(),
            pm_table,
            core_mhz,
            at: TICK,
        }
    }

    fn worker_cpu_ms(&self, core: CoreId, at: Duration) -> i64 {
        let at = if self.stalled == Some(core) {
            at.min(TICK.max(self.ran.saturating_sub(STALL_LEAD)))
        } else {
            at
        };
        i64::try_from(at.as_millis()).unwrap_or(i64::MAX) * i64::from(self.threads)
    }

    /// Writes the samples as JSON lines to `dir/<id>/samples.jsonl`, without holding the series.
    pub(crate) fn persist(&self, dir: &Path, id: &str) -> io::Result<()> {
        let dir = dir.join(id);
        fs::create_dir_all(&dir)?;
        let mut out = BufWriter::new(fs::File::create(dir.join(togi_trialfiles::SAMPLES))?);
        for sample in self.conditions() {
            serde_json::to_writer(&mut out, &sample)?;
            out.write_all(b"\n")?;
        }
        out.flush()
    }
}

pub(crate) struct Conditions {
    trial: LastTrial,
    pm_table: Option<PmTable>,
    core_mhz: PerCore<i64>,
    at: Duration,
}

impl Iterator for Conditions {
    type Item = TrialConditions;

    fn next(&mut self) -> Option<TrialConditions> {
        if self.at >= self.trial.ran {
            return None;
        }
        let at = self.at;
        self.at += TICK;
        Some(TrialConditions {
            elapsed_ms: i64::try_from(at.as_millis()).unwrap_or(i64::MAX),
            worker_cpu_ms: self
                .trial
                .cores
                .iter()
                .map(|core| (core, self.trial.worker_cpu_ms(core, at)))
                .collect(),
            core_mhz: self.core_mhz,
            pm_table: self.pm_table,
            ..TrialConditions::default()
        })
    }
}
