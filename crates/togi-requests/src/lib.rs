//! Summarizes the core voltage requests and clocks a trial recorded and orders loaded cores by
//! request. Both CCDs share one core voltage, and the highest request among the loaded cores sets
//! it.
//!
//! [`summarize`] skips a sample whose loaded-core voltage lanes are not all finite, so the
//! requests it returns are finite. [`groups`] and [`top`] take finite inputs, which come from
//! `summarize` or arithmetic on them.

use std::collections::BTreeMap;
use std::time::Duration;

use togi_machine::{CcdId, CoreId, CoreSet, PER_CORE_MAX, PerCore, TrialConditions};

pub const MIN_SAMPLES: usize = 20;
/// The spread within which cores count as one top.
pub const TIE_V: f64 = 0.001;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Telemetry {
    /// Each loaded core's median request in volts.
    pub requests: PerCore<f64>,
    /// Each CCD's highest-requesting cores, ties included.
    pub top_requesters: CoreSet,
    /// Each loaded CCD's approximate median clock in whole MHz, clamped at the i64 endpoints.
    pub ccd_mhz: BTreeMap<CcdId, i64>,
}

/// Summarizes the loaded `cores`, each on its CCD. Uses the samples at or after `warm_up` that decode request lanes, and
/// returns `None` when fewer than [`MIN_SAMPLES`] remain.
pub fn summarize(
    samples: impl IntoIterator<Item = TrialConditions>,
    cores: &PerCore<Option<CcdId>>,
    warm_up: Duration,
) -> Option<Telemetry> {
    let cores = || cores.iter().filter_map(|(core, ccd)| Some((core, ccd?)));
    cores().next()?;
    let mut requests: [Vec<f64>; PER_CORE_MAX] = Default::default();
    let mut clocks: BTreeMap<CcdId, Vec<f64>> = BTreeMap::new();
    let mut per_ccd: BTreeMap<CcdId, Vec<f64>> = BTreeMap::new();
    let mut count = 0_usize;
    for sample in samples {
        let warmed =
            u64::try_from(sample.elapsed_ms).is_ok_and(|ms| Duration::from_millis(ms) >= warm_up);
        if !warmed {
            continue;
        }
        let Some(table) = &sample.pm_table else {
            continue;
        };
        if !cores().all(|(core, _)| table.voltage_request_v[core.index()].is_finite()) {
            continue;
        }
        for mhz in per_ccd.values_mut() {
            mhz.clear();
        }
        for (core, ccd) in cores() {
            requests[core.index()].push(f64::from(table.voltage_request_v[core.index()]));
            if let Some(mhz) = sample.core_mhz.get(core) {
                per_ccd
                    .entry(ccd)
                    .or_default()
                    .push(approximate_clock_mhz(mhz));
            }
        }
        for (&ccd, mhz) in &mut per_ccd {
            if !mhz.is_empty() {
                clocks.entry(ccd).or_default().push(median(mhz));
            }
        }
        count += 1;
    }
    if count < MIN_SAMPLES {
        return None;
    }
    let mut telemetry = Telemetry::default();
    let mut by_ccd: BTreeMap<CcdId, PerCore<f64>> = BTreeMap::new();
    for (core, ccd) in cores() {
        let value = median(&mut requests[core.index()]);
        telemetry.requests.set(core, value);
        by_ccd.entry(ccd).or_default().set(core, value);
    }
    for group in by_ccd.values() {
        if let Some(top) = groups(group).into_iter().next() {
            telemetry.top_requesters.extend(top);
        }
    }
    for (ccd, mut mhz) in clocks {
        if mhz.len() >= MIN_SAMPLES {
            telemetry
                .ccd_mhz
                .insert(ccd, rounded_clock_mhz(median(&mut mhz)));
        }
    }
    Some(telemetry)
}

/// Orders cores by request, highest first. Each group holds the highest remaining core and every
/// remaining core within [`TIE_V`] of it. Every request must be finite.
pub fn groups(requests: &PerCore<f64>) -> Vec<CoreSet> {
    debug_assert!(requests.iter().all(|(_, request)| request.is_finite()));
    let mut remaining: Vec<(CoreId, f64)> = requests.iter().collect();
    remaining.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut groups = Vec::new();
    let mut rest = remaining.as_slice();
    while let Some(&(_, highest)) = rest.first() {
        let n = 1 + rest[1..]
            .iter()
            .take_while(|(_, request)| *request >= highest - TIE_V)
            .count();
        let (group, tail) = rest.split_at(n);
        groups.push(group.iter().map(|&(core, _)| core).collect());
        rest = tail;
    }
    groups
}

/// The highest request. Every request must be finite.
pub fn top(requests: &PerCore<f64>) -> Option<f64> {
    debug_assert!(requests.iter().all(|(_, request)| request.is_finite()));
    requests.iter().map(|(_, request)| request).reduce(f64::max)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "clock medians are approximate telemetry, including readings above 2^53"
)]
const fn approximate_clock_mhz(value: i64) -> f64 {
    value as f64
}

/// Median inputs are finite approximations of i64 readings; rounding at either endpoint stays there.
#[expect(
    clippy::cast_possible_truncation,
    reason = "approximate clock medians round to whole MHz and clamp rounding at the i64 endpoints"
)]
fn rounded_clock_mhz(value: f64) -> i64 {
    value.round() as i64
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_unstable_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        values[middle - 1].midpoint(values[middle])
    } else {
        values[middle]
    }
}

#[cfg(test)]
mod tests;
