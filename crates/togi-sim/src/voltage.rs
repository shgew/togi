//! The shared-voltage rail of a 16-core machine: package power, CCD clocks, per-core voltage requests
//! and the softplus hazard of each loaded core below its required voltage.
//!
//! Only an R7 trial that loads more than one core is *shared*: the rail then replaces the loaded cores'
//! limit hazards, the joints and the CCD residual. Telemetry for any other trial is computed from the
//! same equations without changing its hazards.

use togi_machine::{CoreId, Regime, TargetOffset, workloads};

use crate::definition::{DEFAULT_COUNT_V, SharedVoltageDef, VoltageCoreDef, VoltageWorkloadDef};
use crate::error::{InvalidMachine, Path, Problem, ShapeError};
use crate::hazard::{Load, SourceKind, steps};
use crate::model::{Model, signal_mix};
use crate::vocab::SignalMix;

const CORES: usize = 16;
const CCD_SIZE: usize = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct Voltage {
    pub idle_v: f64,
    pub margin_v: f64,
    pub rate: f64,
    pub background_rate: f64,
    pub power_limit_w: f64,
    pub thermal_limit_w: f64,
    /// The three R7 workloads in catalog order.
    pub workloads: Vec<VoltageWorkload>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VoltageWorkload {
    pub id: &'static str,
    pub reference_mhz: f64,
    pub full_mhz: [f64; 2],
    pub idle_gain_mhz: f64,
    pub watts_per_core: f64,
    pub offset_watts_per_count: f64,
    pub package_mhz_per_w: f64,
    pub balance_mhz_per_w: f64,
    /// Sixteen cores in core-ID order.
    pub cores: Vec<VoltageCore>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VoltageCore {
    pub base_v: f64,
    pub threshold_v: f64,
    /// Volts per count of offset, with the default applied.
    pub count_v: f64,
    pub clock_v_per_100mhz: f64,
    pub threshold_clock_v_per_100mhz: f64,
    pub signals: Option<SignalMix>,
}

/// The rail at one profile and load.
#[derive(Clone, Debug, PartialEq)]
pub struct VoltageState {
    /// The voltage each core requests; unloaded cores request `idle_v`.
    pub requests: [f32; CORES],
    /// Whole-MHz clock of each CCD, 0 when none of its cores is loaded.
    pub clocks: [u32; 2],
    /// The rail: the highest request of a loaded core.
    pub rail: f64,
    /// Failures per second of each core; 0 for unloaded cores.
    pub rates: [f64; CORES],
}

impl Voltage {
    pub(crate) fn resolve(def: &SharedVoltageDef, path: &Path<'_>) -> Result<Self, InvalidMachine> {
        let table = path.key("workload");
        let catalog = workloads(Regime::R7);
        let resolved = catalog
            .iter()
            .map(|entry| {
                let workload = def
                    .workload
                    .get(entry.id)
                    .ok_or_else(|| table.error(Problem::Workloads))?;
                resolve_workload(entry.id, workload, &table.key(entry.id))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if def.workload.len() != catalog.len() {
            return Err(table.error(Problem::Workloads));
        }
        Ok(Self {
            idle_v: path.key("idle_v").positive(def.idle_v)?,
            margin_v: path.key("margin_v").positive(def.margin_v)?,
            rate: path.key("rate").positive(def.rate)?,
            background_rate: path
                .key("background_rate")
                .nonnegative(def.background_rate)?,
            power_limit_w: path.key("power_limit_w").positive(def.power_limit_w)?,
            thermal_limit_w: path.key("thermal_limit_w").positive(def.thermal_limit_w)?,
            workloads: resolved,
        })
    }

    /// The parameters of workload `id`; an ID that is not an R7 workload gets the first R7 workload's,
    /// which makes telemetry for other regimes illustrative.
    pub fn workload(&self, id: &str) -> &VoltageWorkload {
        self.workloads
            .iter()
            .find(|workload| workload.id == id)
            .unwrap_or(&self.workloads[0])
    }
}

fn resolve_workload(
    id: &'static str,
    def: &VoltageWorkloadDef,
    path: &Path<'_>,
) -> Result<VoltageWorkload, InvalidMachine> {
    let full = path.key("full_mhz");
    let cores = path.key("core");
    cores.length(CORES, def.core.len())?;
    Ok(VoltageWorkload {
        id,
        reference_mhz: path.key("reference_mhz").positive(def.reference_mhz)?,
        full_mhz: [
            full.index(0).positive(def.full_mhz[0])?,
            full.index(1).positive(def.full_mhz[1])?,
        ],
        idle_gain_mhz: path.key("idle_gain_mhz").nonnegative(def.idle_gain_mhz)?,
        watts_per_core: path.key("watts_per_core").positive(def.watts_per_core)?,
        offset_watts_per_count: path
            .key("offset_watts_per_count")
            .nonnegative(def.offset_watts_per_count)?,
        package_mhz_per_w: path
            .key("package_mhz_per_w")
            .nonnegative(def.package_mhz_per_w)?,
        balance_mhz_per_w: path
            .key("balance_mhz_per_w")
            .nonnegative(def.balance_mhz_per_w)?,
        cores: def
            .core
            .iter()
            .enumerate()
            .map(|(i, core)| resolve_core(core, &cores.index(i)))
            .collect::<Result<_, _>>()?,
    })
}

fn resolve_core(def: &VoltageCoreDef, path: &Path<'_>) -> Result<VoltageCore, InvalidMachine> {
    let count_v = if def.count_v == 0.0 {
        DEFAULT_COUNT_V
    } else {
        def.count_v
    };
    Ok(VoltageCore {
        base_v: path.key("base_v").positive(def.base_v)?,
        threshold_v: path.key("threshold_v").positive(def.threshold_v)?,
        count_v: path.key("count_v").positive(count_v)?,
        clock_v_per_100mhz: path
            .key("clock_v_per_100mhz")
            .nonnegative(def.clock_v_per_100mhz)?,
        threshold_clock_v_per_100mhz: path
            .key("threshold_clock_v_per_100mhz")
            .nonnegative(def.threshold_clock_v_per_100mhz)?,
        signals: def
            .signals
            .as_ref()
            .map(|signals| signal_mix(&path.key("signals"), signals))
            .transpose()?,
    })
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded, at least 1 and saturates at u32::MAX, a clock no machine reaches"
)]
fn whole_mhz(mhz: f64) -> u32 {
    mhz.round().max(1.0) as u32
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the narrower request is the recorded telemetry value"
)]
fn narrow(volts: f64) -> f32 {
    volts as f32
}

impl Model {
    /// The rail for `profile` and `load` on a shared-voltage machine, whatever the regime, or `None`
    /// when the machine has no shared voltage.
    pub fn voltage_state(
        &self,
        profile: &[TargetOffset],
        load: &Load<'_>,
    ) -> Result<Option<VoltageState>, ShapeError> {
        self.check_shape(profile, load)?;
        Ok(self
            .voltage()
            .map(|voltage| voltage_state(voltage, profile, load)))
    }

    /// The voltage each core requests, for a multi-core R7 load on a shared-voltage machine only.
    pub fn r7_requests(
        &self,
        profile: &[TargetOffset],
        load: &Load<'_>,
    ) -> Result<Option<[f32; CORES]>, ShapeError> {
        self.check_shape(profile, load)?;
        Ok(self
            .shared_voltage(load)
            .map(|voltage| voltage_state(voltage, profile, load).requests))
    }

    /// The loaded core's shared-voltage rate over the total steady-state hazard, including the
    /// background and unloaded-core hazards. Zero outside a multi-core shared-voltage R7 load, for a core
    /// that is not loaded, and when the total hazard is zero.
    pub fn r7_failure_share(
        &self,
        profile: &[TargetOffset],
        load: &Load<'_>,
        core: CoreId,
    ) -> Result<f64, ShapeError> {
        self.check_shape(profile, load)?;
        if self.shared_voltage(load).is_none() || !load.cores.contains(core) {
            return Ok(0.0);
        }
        let (mut total, mut own) = (0.0, 0.0);
        self.visit_sources(profile, load, |source| {
            total += source.rate;
            if source.kind == SourceKind::Voltage(core) {
                own = source.rate;
            }
        })?;
        Ok(if total > 0.0 { own / total } else { 0.0 })
    }
}

pub(crate) fn voltage_state(
    voltage: &Voltage,
    profile: &[TargetOffset],
    load: &Load<'_>,
) -> VoltageState {
    let workload = voltage.workload(load.workload);
    let mut loaded = [0u32; 2];
    let mut watts = [0.0f64; 2];
    for core in load.cores {
        let ccd = core.index() / CCD_SIZE;
        loaded[ccd] += 1;
        let counts = steps(profile[core.index()].get() + 35);
        watts[ccd] += (workload.watts_per_core + workload.offset_watts_per_count * counts).max(0.0);
    }
    let package = watts[0] + watts[1];
    let pressure = (package - voltage.power_limit_w.min(voltage.thermal_limit_w)).max(0.0);
    let mut clocks = [0u32; 2];
    for ccd in 0..2 {
        if loaded[ccd] == 0 {
            continue;
        }
        let mut mhz = workload.full_mhz[ccd] + workload.idle_gain_mhz * f64::from(8 - loaded[ccd])
            - workload.package_mhz_per_w * pressure;
        if loaded[0] > 0 && loaded[1] > 0 {
            mhz -= workload.balance_mhz_per_w * (watts[ccd] - package / 2.0);
        }
        clocks[ccd] = whole_mhz(mhz);
    }
    let mut requests = [narrow(voltage.idle_v); CORES];
    let mut rail = 0.0f64;
    let above_reference = |core: usize| f64::from(clocks[core / CCD_SIZE]) - workload.reference_mhz;
    for core in load.cores {
        let index = core.index();
        let parameters = &workload.cores[index];
        let request = parameters.base_v
            + parameters.count_v * steps(profile[index].get())
            + parameters.clock_v_per_100mhz * above_reference(index) / 100.0;
        requests[index] = narrow(request);
        rail = rail.max(request);
    }
    let mut rates = [0.0; CORES];
    for core in load.cores {
        let index = core.index();
        let parameters = &workload.cores[index];
        let mut threshold = parameters.threshold_v;
        if parameters.threshold_clock_v_per_100mhz != 0.0 {
            threshold += parameters.threshold_clock_v_per_100mhz * above_reference(index) / 100.0;
        }
        let x = (threshold - rail) / voltage.margin_v;
        let softplus = x.max(0.0) + (-x.abs()).exp().ln_1p();
        rates[index] = voltage.rate * softplus / std::f64::consts::LN_2;
    }
    VoltageState {
        requests,
        clocks,
        rail,
        rates,
    }
}
