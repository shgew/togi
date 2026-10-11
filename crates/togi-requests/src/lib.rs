//! Summarizes the core voltage requests and clocks a trial recorded and orders loaded cores by
//! request. Both CCDs share one core voltage, and the highest request among the loaded cores sets
//! it.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use togi_gocompat::convert::{ceil, f64_to_i64, round};
use togi_gocompat::sort::sort_f64;
use togi_machine::{CcdId, CoreId, PER_CORE_MAX, PerCore, TrialConditions};

/// Skips the samples taken while the load ramps up.
pub const WARMUP_MS: i64 = 5000;
pub const MIN_SAMPLES: usize = 20;
/// The spread within which cores count as one top.
pub const TIE_V: f64 = 0.001;
pub const VOLTS_PER_COUNT: f64 = 0.0036;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Telemetry {
    /// Each loaded core's median request in volts.
    pub requests: PerCore<f64>,
    /// Each CCD's highest-requesting cores, ties included.
    pub top_requesters: Vec<CoreId>,
    /// Each loaded CCD's median over samples of its loaded cores' median clock.
    pub ccd_mhz: BTreeMap<CcdId, i64>,
}

/// Uses the samples after the warmup that decode request lanes, and returns `None` when fewer than
/// [`MIN_SAMPLES`] remain.
pub fn summarize(
    samples: impl IntoIterator<Item = TrialConditions>,
    cores: &[CoreId],
    ccd_of: impl Fn(CoreId) -> CcdId,
) -> Option<Telemetry> {
    if cores.is_empty() {
        return None;
    }
    let mut requests: [Vec<f64>; PER_CORE_MAX] = Default::default();
    let mut clocks: BTreeMap<CcdId, Vec<f64>> = BTreeMap::new();
    let mut per_ccd: BTreeMap<CcdId, Vec<f64>> = BTreeMap::new();
    let mut count = 0_usize;
    for sample in samples {
        if sample.elapsed_ms < WARMUP_MS {
            continue;
        }
        let Some(table) = &sample.pm_table else {
            continue;
        };
        for mhz in per_ccd.values_mut() {
            mhz.clear();
        }
        for &core in cores {
            requests[core.index()].push(f64::from(table.voltage_request_v[core.index()]));
            if let Some(mhz) = sample.core_mhz.get(core) {
                per_ccd.entry(ccd_of(core)).or_default().push(to_f64(mhz));
            }
        }
        for (&ccd, mhz) in &mut per_ccd {
            if !mhz.is_empty() {
                clocks.entry(ccd).or_default().push(median_in_place(mhz));
            }
        }
        count += 1;
    }
    if count < MIN_SAMPLES {
        return None;
    }
    let mut telemetry = Telemetry::default();
    let mut by_ccd: BTreeMap<CcdId, PerCore<f64>> = BTreeMap::new();
    for &core in cores {
        if telemetry.requests.get(core).is_some() {
            continue;
        }
        let value = median(&requests[core.index()]);
        telemetry.requests.set(core, value);
        by_ccd.entry(ccd_of(core)).or_default().set(core, value);
    }
    for group in by_ccd.values() {
        if let Some(top) = groups(group).into_iter().next() {
            telemetry.top_requesters.extend(top);
        }
    }
    telemetry.top_requesters.sort_unstable();
    for (ccd, mhz) in clocks {
        if mhz.len() >= MIN_SAMPLES {
            telemetry
                .ccd_mhz
                .insert(ccd, f64_to_i64(round(median(&mhz))));
        }
    }
    Some(telemetry)
}

/// Orders cores by request, highest first. Each group holds the highest remaining core and every
/// remaining core within [`TIE_V`] of it, sorted by core.
pub fn groups(requests: &PerCore<f64>) -> Vec<Vec<CoreId>> {
    let mut remaining: Vec<(CoreId, f64)> = requests.all().collect();
    remaining.sort_by(|a, b| compare(b.1, a.1).then(a.0.cmp(&b.0)));
    let mut groups = Vec::new();
    let mut rest = remaining.as_slice();
    while let Some(&(_, highest)) = rest.first() {
        let n = 1 + rest[1..]
            .iter()
            .take_while(|(_, request)| *request >= highest - TIE_V)
            .count();
        let (group, tail) = rest.split_at(n);
        let mut cores: Vec<CoreId> = group.iter().map(|&(core, _)| core).collect();
        cores.sort_unstable();
        groups.push(cores);
        rest = tail;
    }
    groups
}

/// The counts shallower that raise a request from `v` to at least `target`, and at least one.
pub fn counts(v: f64, target: f64) -> i64 {
    f64_to_i64(ceil((target - v) / VOLTS_PER_COUNT - 1e-9)).max(1)
}

pub fn top(requests: &PerCore<f64>) -> Option<f64> {
    requests.all().map(|(_, request)| request).reduce(go_max)
}

fn compare(x: f64, y: f64) -> Ordering {
    let (x_nan, y_nan) = (x.is_nan(), y.is_nan());
    if (x_nan && !y_nan) || x < y {
        Ordering::Less
    } else if x > y || (y_nan && !x_nan) {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

fn go_max(x: f64, y: f64) -> f64 {
    if x.is_nan() {
        x
    } else if y.is_nan() {
        y
    } else if x > y {
        x
    } else if y > x || x.is_sign_negative() {
        y
    } else {
        x
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Go's float64(int) rounds the same way"
)]
const fn to_f64(value: i64) -> f64 {
    value as f64
}

fn median(values: &[f64]) -> f64 {
    median_in_place(&mut values.to_vec())
}

fn median_in_place(values: &mut [f64]) -> f64 {
    sort_f64(values);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        #[expect(
            clippy::manual_midpoint,
            reason = "Go adds before halving, which f64::midpoint does not at the extremes"
        )]
        let mean = (values[middle - 1] + values[middle]) / 2.0;
        mean
    } else {
        values[middle]
    }
}

#[cfg(test)]
mod tests;
