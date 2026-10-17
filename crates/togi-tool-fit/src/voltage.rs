//! The shared-voltage model of the fit: starting values from a sample's own request and clock telemetry,
//! the prior, the named-core attribution term and the voltage coordinate search.

use std::collections::BTreeMap;

use togi_facts::{Decisive, TrialClass};
use togi_machine::{CcdId, CoreId, Regime, TargetOffset, pick_workload, workloads};
use togi_sim::definition::{SharedVoltageDef, VoltageCoreDef, VoltageWorkloadDef};
use togi_sim::voltage::core_watts;
use togi_sim::{MachineDefinition, Model, saturating_count};
use togi_tool_trialfacts::{Profile, TrialFact};

use crate::evidence::{load_of, regime_of, trial_class};
use crate::likelihood::Likelihood;

const REFERENCE_MHZ: f64 = 5240.0;
const CORES: usize = 16;

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if !n.is_multiple_of(2) {
        values[n / 2]
    } else {
        f64::midpoint(values[n / 2 - 1], values[n / 2])
    }
}

struct RequestPoint {
    offset: f64,
    clock: f64,
    voltage: f64,
}

fn r7_ids() -> impl Iterator<Item = &'static str> {
    workloads(Regime::R7).iter().map(|w| w.id)
}

fn float(value: i64) -> f64 {
    f64::from(i32::try_from(value).unwrap_or(0))
}

pub(crate) fn voltage_config(records: &[TrialFact]) -> SharedVoltageDef {
    let mut v = SharedVoltageDef {
        idle_v: 0.8,
        margin_v: 0.006,
        rate: 0.001,
        background_rate: 0.0,
        power_limit_w: 192.0,
        thermal_limit_w: 192.0,
        workload: BTreeMap::new(),
    };
    for (index, workload) in workloads(Regime::R7).iter().enumerate() {
        let mut w = VoltageWorkloadDef {
            reference_mhz: REFERENCE_MHZ,
            full_mhz: [5240.0, 5240.0],
            idle_gain_mhz: 30.0,
            watts_per_core: 13.0,
            offset_watts_per_count: 0.12,
            package_mhz_per_w: 13.0,
            balance_mhz_per_w: 19.3,
            core: vec![
                VoltageCoreDef {
                    base_v: 1.25,
                    threshold_v: 1.15,
                    count_v: 0.0036,
                    clock_v_per_100mhz: 0.033,
                    threshold_clock_v_per_100mhz: 0.033,
                    signals: None,
                };
                CORES
            ],
        };
        if index != 0 {
            w.full_mhz = [5070.0, 5070.0];
            w.watts_per_core = 15.0;
            w.package_mhz_per_w = 5.65;
            w.balance_mhz_per_w = 16.67;
        }
        v.workload.insert(workload.id.to_owned(), w);
    }
    let avx2 = pick_workload(Regime::R7, 0).id;
    fit_requests(&mut v, records, avx2);
    for id in r7_ids().skip(1) {
        let prior = v.workload[avx2].core.clone();
        if let Some(w) = v.workload.get_mut(id) {
            w.core = prior;
        }
        fit_requests(&mut v, records, id);
    }
    for id in r7_ids() {
        fit_clocks(&mut v, records, id);
    }
    v
}

#[expect(
    clippy::many_single_char_names,
    reason = "the normal equations of a two-variable ridge regression"
)]
fn fit_requests(v: &mut SharedVoltageDef, records: &[TrialFact], id: &str) {
    let Some(w) = v.workload.get_mut(id) else {
        return;
    };
    let mut points: [Vec<RequestPoint>; CORES] = std::array::from_fn(|_| Vec::new());
    for fact in records {
        let Some(class) = trial_class(fact) else {
            continue;
        };
        if class.workload.regime() != Regime::R7 || class.workload.as_str() != id {
            continue;
        }
        for (core, slot) in points.iter_mut().enumerate() {
            let Ok(core_id) = CoreId::try_from(core) else {
                continue;
            };
            let (Some(voltage), Some(&mhz), Some(offset)) = (
                fact.voltage_requests_v.get(core_id),
                CcdId::try_from(core / 8)
                    .ok()
                    .and_then(|ccd| fact.ccd_mhz.get(&ccd)),
                fact.profile.as_slice().get(core),
            ) else {
                continue;
            };
            if !class.loaded.contains(core_id) {
                continue;
            }
            slot.push(RequestPoint {
                offset: float(offset.get()),
                clock: (f64::from(mhz) - w.reference_mhz) / 100.0,
                voltage,
            });
        }
    }
    // Within-core centering removes the intercept. Ridge terms express voltage residual SD 2mV and prior
    // SDs .5mV/count and 8mV/100MHz.
    let (mut a, mut b, mut d) = (16.0, 0.0, 0.0625);
    let (mut e, mut f) = (a * 0.0036, d * 0.033);
    let mut total = 0;
    for ps in &points {
        if ps.is_empty() {
            continue;
        }
        total += ps.len();
        let n = saturating_count(ps.len());
        let x = ps.iter().map(|p| p.offset).sum::<f64>() / n;
        let y = ps.iter().map(|p| p.clock).sum::<f64>() / n;
        let z = ps.iter().map(|p| p.voltage).sum::<f64>() / n;
        for p in ps {
            let (xx, yy, zz) = (p.offset - x, p.clock - y, p.voltage - z);
            a += xx * xx;
            b += xx * yy;
            d += yy * yy;
            e += xx * zz;
            f += yy * zz;
        }
    }
    if total == 0 {
        return;
    }
    let count_v = ((e * d - f * b) / (a * d - b * b)).clamp(0.0025, 0.005);
    let clock_v = ((a * f - b * e) / (a * d - b * b)).clamp(0.005, 0.06);
    let mut bases = [0.0; CORES];
    let mut supported = [false; CORES];
    for (core, ps) in points.iter().enumerate() {
        if ps.is_empty() {
            continue;
        }
        let mut residuals: Vec<f64> = ps
            .iter()
            .map(|p| p.voltage - count_v * p.offset - clock_v * p.clock)
            .collect();
        bases[core] = median(&mut residuals);
        supported[core] = true;
    }
    for ccd in 0..2 {
        let range = ccd * 8..(ccd + 1) * 8;
        let mut values = [0.0; 8];
        let mut len = 0;
        for core in range.clone().filter(|&core| supported[core]) {
            values[len] = bases[core];
            len += 1;
        }
        if len == 0 {
            continue;
        }
        let mean = median(&mut values[..len]);
        for core in range {
            let n = saturating_count(points[core].len());
            let c = &mut w.core[core];
            c.base_v = ((n * bases[core] + 5.0 * mean) / (n + 5.0)).clamp(0.9, 1.5);
            c.count_v = count_v;
            c.clock_v_per_100mhz = clock_v;
            c.threshold_clock_v_per_100mhz = clock_v;
            c.threshold_v = c.base_v - 0.1;
        }
    }
}

fn fit_clocks(v: &mut SharedVoltageDef, records: &[TrialFact], id: &str) {
    let limit = v.power_limit_w.min(v.thermal_limit_w);
    let Some(w) = v.workload.get_mut(id) else {
        return;
    };
    let mut full: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    let mut partial: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
    let r7: Vec<(&TrialFact, TrialClass)> = records
        .iter()
        .filter_map(|fact| trial_class(fact).map(|class| (fact, class)))
        .filter(|(_, class)| class.workload.regime() == Regime::R7 && class.workload.as_str() == id)
        .collect();
    for (fact, class) in &r7 {
        let mut counts = [0_usize; 2];
        let mut zero = [false; 2];
        for core in class.loaded {
            let ccd = core.index() / 8;
            counts[ccd] += 1;
            zero[ccd] = zero[ccd]
                || fact
                    .profile
                    .as_slice()
                    .get(core.index())
                    .is_some_and(|offset| offset.get() == 0);
        }
        // Parked zero-offset cores are power-limited and must not lower the unconstrained full-CCD
        // intercept. All-core pressure is fitted below.
        for (ccd, &clock) in &fact.ccd_mhz {
            let ccd = usize::from(ccd.get());
            if ccd > 1 || zero[ccd] || counts[1 - ccd] > 0 {
                continue;
            }
            match counts[ccd] {
                8 => full[ccd].push(f64::from(clock)),
                7 => partial[ccd].push(f64::from(clock)),
                _ => {}
            }
        }
    }
    let mut gains = Vec::new();
    for ccd in 0..2 {
        if !full[ccd].is_empty() {
            w.full_mhz[ccd] = median(&mut full[ccd]).clamp(4000.0, 6000.0);
        }
        if !full[ccd].is_empty() && !partial[ccd].is_empty() {
            gains.push(median(&mut partial[ccd]) - w.full_mhz[ccd]);
        }
    }
    if !gains.is_empty() {
        w.idle_gain_mhz = median(&mut gains).clamp(0.0, 60.0);
    }
    // Fix the watt scale; clocks identify MHz/W responses, not watts and coefficients separately. Fit
    // package/balance responses only when seen.
    let mut package = Vec::new();
    let mut balance = Vec::new();
    for (fact, class) in &r7 {
        let clock = |id| {
            CcdId::new(id)
                .ok()
                .and_then(|ccd| fact.ccd_mhz.get(&ccd).copied())
        };
        let (Some(c0), Some(c1)) = (clock(0), clock(1)) else {
            continue;
        };
        if class.loaded.len() != CORES || fact.ccd_mhz.len() != 2 {
            continue;
        }
        let mut watts = [0.0_f64; 2];
        for core in class.loaded {
            let offset = fact
                .profile
                .as_slice()
                .get(core.index())
                .copied()
                .unwrap_or(TargetOffset::ZERO);
            if let Some(slot) = watts.get_mut(core.index() / 8) {
                *slot += core_watts(w.watts_per_core, w.offset_watts_per_count, offset);
            }
        }
        let (c0, c1) = (f64::from(c0), f64::from(c1));
        let pressure = (watts[0] + watts[1] - limit).max(0.0);
        if pressure > 1.0 {
            package.push((w.full_mhz[0] + w.full_mhz[1] - (c0 + c1)) / (2.0 * pressure));
        }
        if (watts[0] - watts[1]).abs() > 1.0 {
            balance.push((w.full_mhz[0] - w.full_mhz[1] - (c0 - c1)) / (watts[0] - watts[1]));
        }
    }
    if !package.is_empty() {
        w.package_mhz_per_w = median(&mut package).clamp(0.0, 30.0);
    }
    if !balance.is_empty() {
        w.balance_mhz_per_w = median(&mut balance).clamp(0.0, 30.0);
    }
}

/// The prior of the shared-voltage model: thresholds of one CCD and workload stay within about 10 mV of
/// their mean, and the margin width stays near 6 mV within a factor of two.
pub(crate) fn voltage_penalty(v: &SharedVoltageDef) -> f64 {
    let mut penalty = 0.0;
    for id in r7_ids().take(2) {
        let Some(w) = v.workload.get(id) else {
            continue;
        };
        for ccd in 0..2 {
            let thresholds = &w.core[ccd * 8..(ccd + 1) * 8];
            let mean: f64 = thresholds.iter().map(|c| c.threshold_v / 8.0).sum();
            for core in thresholds {
                let deviation = (core.threshold_v - mean) / 0.01;
                penalty += 0.5 * deviation * deviation;
            }
        }
    }
    let width = (v.margin_v / 0.006).ln() / std::f64::consts::LN_2;
    penalty + 0.5 * width * width
}

/// A failure that names a loaded core of a multi-core R7 trial.
pub(crate) struct NamedFailure {
    pub(crate) profile: Profile,
    pub(crate) class: TrialClass,
    pub(crate) core: CoreId,
}

pub(crate) fn named_failures(records: &[TrialFact]) -> Vec<NamedFailure> {
    let mut named = Vec::new();
    for fact in records {
        let Some(class) = trial_class(fact) else {
            continue;
        };
        if regime_of(fact) != Regime::R7 || class.loaded.len() < 2 {
            continue;
        }
        let Decisive::Failure { core, stalled, .. } = fact.outcome else {
            continue;
        };
        let Some(core) = core.or(stalled) else {
            continue;
        };
        if class.loaded.contains(core) {
            named.push(NamedFailure {
                profile: fact.profile,
                class,
                core,
            });
        }
    }
    named
}

/// Conditional on a failure, competing stationary hazards identify its core. All anchor hazards have the
/// same onset exposure; signal weights are fixed.
pub(crate) fn named_loss(model: &Model, named: &[NamedFailure]) -> f64 {
    let mut loss = 0.0;
    for failure in named {
        let share = model
            .r7_failure_share(
                failure.profile.as_slice(),
                &load_of(&failure.class),
                failure.core,
            )
            .unwrap_or(0.0);
        if share <= 0.0 {
            return f64::INFINITY;
        }
        loss -= share.ln();
    }
    loss
}

pub(crate) fn sync_ycruncher(v: &mut SharedVoltageDef) {
    let mut ids = r7_ids();
    let (Some(a), Some(b), Some(y)) = (ids.next(), ids.next(), ids.next()) else {
        return;
    };
    let (Some(a), Some(b)) = (v.workload.get(a), v.workload.get(b)) else {
        return;
    };
    let mut thresholds = [0.0; CORES];
    let mut len = 0;
    for (slot, (a, b)) in thresholds.iter_mut().zip(a.core.iter().zip(&b.core)) {
        *slot = a.threshold_v.max(b.threshold_v);
        len += 1;
    }
    if let Some(y) = v.workload.get_mut(y) {
        for (core, &threshold) in y.core.iter_mut().zip(&thresholds[..len]) {
            core.threshold_v = threshold;
        }
    }
}

fn voltage_of(def: &mut MachineDefinition) -> Option<&mut SharedVoltageDef> {
    def.shared_voltage.as_mut()
}

impl Likelihood<'_> {
    /// Fits the shared-voltage parameters: thresholds, then rate, margin and background, each minimizing
    /// the shared augmented objective on the multi-core R7 observations.
    pub(crate) fn fit_voltage(&mut self) {
        let indices = self.select(|o| o.regime() == Regime::R7 && o.class.loaded.len() > 1);
        if indices.is_empty() {
            return;
        }
        for id in r7_ids().take(2) {
            for ccd in 0..2 {
                let original: Option<[f64; 8]> = self.cfg.shared_voltage.as_ref().map(|v| {
                    let cores = &v.workload[id].core[ccd * 8..(ccd + 1) * 8];
                    std::array::from_fn(|core| cores[core].threshold_v)
                });
                let low = original
                    .iter()
                    .flatten()
                    .fold(-0.10_f64, |low, t| low.max(0.8 - t));
                let high = original
                    .iter()
                    .flatten()
                    .fold(0.10_f64, |high, t| high.min(1.5 - t));
                self.linear(
                    &indices,
                    0.0,
                    |d, x| {
                        if let Some(v) = voltage_of(d) {
                            if let Some(w) = v.workload.get_mut(id) {
                                for (core, base) in w.core[ccd * 8..(ccd + 1) * 8]
                                    .iter_mut()
                                    .zip(original.iter().flatten())
                                {
                                    core.threshold_v = base + x;
                                }
                            }
                            sync_ycruncher(v);
                        }
                    },
                    low,
                    high,
                );
            }
            for core in 0..CORES {
                let current = self
                    .cfg
                    .shared_voltage
                    .as_ref()
                    .map_or(0.0, |v| v.workload[id].core[core].threshold_v);
                self.linear(
                    &indices,
                    current,
                    |d, x| {
                        if let Some(v) = voltage_of(d) {
                            if let Some(w) = v.workload.get_mut(id) {
                                w.core[core].threshold_v = x;
                            }
                            sync_ycruncher(v);
                        }
                    },
                    (current - 0.03).max(0.8),
                    (current + 0.03).min(1.5),
                );
            }
        }
        let Some((rate, margin, background)) = self
            .cfg
            .shared_voltage
            .as_ref()
            .map(|v| (v.rate, v.margin_v, v.background_rate))
        else {
            return;
        };
        self.linear(
            &indices,
            rate.ln(),
            |d, x| {
                if let Some(v) = voltage_of(d) {
                    v.rate = x.exp();
                    sync_ycruncher(v);
                }
            },
            1e-5_f64.ln(),
            0.03_f64.ln(),
        );
        self.linear(
            &indices,
            margin.ln(),
            |d, x| {
                if let Some(v) = voltage_of(d) {
                    v.margin_v = x.exp();
                    sync_ycruncher(v);
                }
            },
            0.001_f64.ln(),
            0.03_f64.ln(),
        );
        self.linear(
            &indices,
            background,
            |d, x| {
                if let Some(v) = voltage_of(d) {
                    v.background_rate = x;
                    sync_ycruncher(v);
                }
            },
            0.0,
            0.001,
        );
    }
}
