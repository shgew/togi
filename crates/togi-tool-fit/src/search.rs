//! The coordinate search: each step fits one group of parameters with the others held.
//!
//! There is one fitting path. Every sample, whether all facts, a bootstrap draw or a forward-check prefix,
//! is fitted with the shared-voltage model for multi-core R7 and per-core limits for everything else,
//! and every step of it minimizes the same augmented objective.

use std::collections::BTreeSet;

use togi_machine::{REGIMES, Regime, WorkloadId};
use togi_sim::{MachineDefinition, Model};
use togi_tool_modelcheck::Checker;
use togi_tool_trialfacts::TrialFact;

use crate::error::Error;
use crate::evidence::{DEEPEST, Observation, aggregate, initial_config};
use crate::likelihood::Likelihood;
use crate::signals::fit_signals;
use crate::voltage::{named_failures, sync_ycruncher, voltage_config};

const SWEEPS: usize = 12;
const CONVERGED: f64 = 1e-5;
const MIN_WORKLOAD_TRIALS: u32 = 10;
const LIMIT_RANGE: (i64, i64) = (DEEPEST, 1);
const CORES: usize = 16;

/// Fits the records and returns the machine with its raw negative log likelihood.
pub(crate) fn fit(records: &[&TrialFact]) -> Result<(MachineDefinition, f64), Error> {
    fit_from(records, None, None)
}

/// [`fit`] starting from `initial` rather than the default start, and, with a `guard`, keeping only models
/// the model check admits.
///
/// The default start is the sample's own: the shared-voltage model is initialized from the request and
/// clock telemetry of `records`, so a bootstrap draw starts from the telemetry its trials carry. Flat
/// rates and idle limits are fitted on the observations the per-core mechanism explains, never on a
/// multi-core R7 load. No joint and no residual hazard is ever fitted. The returned loss is the raw
/// Bernoulli loss, without the prior and named-core terms every step minimized.
pub(crate) fn fit_from(
    records: &[&TrialFact],
    initial: Option<&MachineDefinition>,
    guard: Option<&Checker>,
) -> Result<(MachineDefinition, f64), Error> {
    for fact in records {
        let cores = fact.profile.as_slice().len();
        if cores != CORES {
            return Err(Error::NeedsSixteenCores {
                session: fact.session,
                seq: fact.seq,
                cores,
            });
        }
    }
    let mut cfg = if let Some(initial) = initial {
        initial.clone()
    } else {
        let mut cfg = initial_config(records);
        let mut shared = voltage_config(records);
        sync_ycruncher(&mut shared);
        cfg.shared_voltage = Some(shared);
        cfg
    };
    fit_signals(&mut cfg, records);
    let obs = aggregate(records);
    let named = named_failures(records);
    let mut l = Likelihood::new(cfg, &obs, guard, Some(&named));
    let all = l.all();
    let mut previous = f64::INFINITY;
    for _ in 0..SWEEPS {
        l.fit_voltage();
        l.fit_regime_limits();
        l.fit_workloads();
        l.fit_hazard_shape(&all);
        l.fit_flat();
        l.fit_idle();
        l.fit_limit_rate_shift(&all);
        let score = l.score(&all);
        if previous - score < CONVERGED {
            break;
        }
        previous = score;
    }
    Model::new(&l.cfg, 0)?;
    let loss = l.raw_score(&all);
    Ok((l.cfg, loss))
}

/// Whether per-core limits, rather than the shared-voltage model, explain `o`: a multi-core R7 load on a
/// machine with the rail is explained by the rail alone.
fn limit_driven(rail: bool, o: &Observation) -> bool {
    !(rail && o.regime() == Regime::R7 && o.class.loaded.len() > 1)
}

fn offset_of(o: &Observation, core: usize) -> i64 {
    o.profile
        .as_slice()
        .get(core)
        .map_or(0, |offset| offset.get())
}

impl Likelihood<'_> {
    pub(crate) fn fit_regime_limits(&mut self) {
        let rail = self.cfg.shared_voltage.is_some();
        for (core, id) in self.core_ids().enumerate() {
            for (r, regime) in REGIMES.into_iter().enumerate() {
                for alone in [true, false] {
                    if alone && r >= 5 {
                        continue;
                    }
                    let indices = self.select(|o| {
                        if !limit_driven(rail, o) || o.regime() != regime || !o.loads(id) {
                            return false;
                        }
                        let only = r < 5
                            && o.profile
                                .as_slice()
                                .iter()
                                .enumerate()
                                .all(|(c, offset)| c == core || offset.get() == 0);
                        only == alone
                    });
                    if alone {
                        self.discrete(
                            |d| &mut d.core[core].alone[r],
                            &indices,
                            LIMIT_RANGE.0,
                            LIMIT_RANGE.1,
                        );
                    } else {
                        self.discrete(
                            |d| &mut d.core[core].together[r],
                            &indices,
                            LIMIT_RANGE.0,
                            LIMIT_RANGE.1,
                        );
                    }
                }
            }
        }
    }

    pub(crate) fn fit_hazard_shape(&mut self, all: &[usize]) {
        self.continuous(
            all,
            |d| d.model.past_limit_rate,
            |d, x| d.model.past_limit_rate = x,
            1e-8,
            0.5,
        );
        self.continuous(
            all,
            |d| d.model.growth,
            |d, x| d.model.growth = x.max(1.05),
            1.05,
            8.0,
        );
        self.continuous(
            all,
            |d| d.model.near_limit_rate,
            |d, x| d.model.near_limit_rate = x,
            1e-8,
            0.001,
        );
    }

    /// Fits each core's flat rate on the observations its limits explain, so a multi-core R7 load, which
    /// the rail alone explains, never moves it.
    pub(crate) fn fit_flat(&mut self) {
        let rail = self.cfg.shared_voltage.is_some();
        for core in 0..self.cores() {
            let indices = self.select(|o| limit_driven(rail, o) && offset_of(o, core) < 0);
            self.continuous(
                &indices,
                |d| d.core[core].flat,
                |d, x| d.core[core].flat = x,
                1e-10,
                0.001,
            );
        }
    }

    /// Fits each core's idle limit on the observations its limits explain; a multi-core R7 load's unloaded
    /// cores are left to the rail model.
    pub(crate) fn fit_idle(&mut self) {
        let rail = self.cfg.shared_voltage.is_some();
        for (core, id) in self.core_ids().enumerate() {
            let indices = self.select(|o| limit_driven(rail, o) && !o.loads(id));
            let exposed = indices.iter().any(|&i| offset_of(&self.obs[i], core) < 0);
            let idle = self.cfg.core[core].idle.unwrap_or(DEEPEST);
            self.cfg.core[core].idle = Some(idle);
            self.rebuild();
            if exposed {
                self.discrete(
                    |d| d.core[core].idle.get_or_insert(DEEPEST),
                    &indices,
                    LIMIT_RANGE.0,
                    LIMIT_RANGE.1,
                );
            }
            if self.cfg.core[core].idle == Some(DEEPEST) {
                self.cfg.core[core].idle = None;
                self.rebuild();
            }
        }
    }

    pub(crate) fn fit_workloads(&mut self) {
        let rail = self.cfg.shared_voltage.is_some();
        for (core, id) in self.core_ids().enumerate() {
            let workloads: BTreeSet<(&'static str, WorkloadId)> = self
                .obs
                .iter()
                .filter(|o| limit_driven(rail, o) && o.k > 0 && o.loads(id))
                .map(|o| (o.class.workload.as_str(), o.class.workload))
                .collect();
            for (_, workload) in workloads {
                let indices = self.select(|o| {
                    limit_driven(rail, o) && o.class.workload == workload && o.loads(id)
                });
                let trials: u32 = indices.iter().map(|&i| self.obs[i].n).sum();
                if trials < MIN_WORKLOAD_TRIALS {
                    continue;
                }
                let baseline = self.score(&indices);
                let existing = self.cfg.core[core].workload.get(&workload).copied();
                let mut best = existing.unwrap_or(DEEPEST);
                let mut score = f64::INFINITY;
                let values: Vec<i64> = (LIMIT_RANGE.0..=LIMIT_RANGE.1).collect();
                let candidates = self.scan(
                    &values,
                    |d, &value| {
                        d.core[core].workload.insert(workload, value);
                    },
                    &indices,
                );
                for (candidate, &value) in candidates.iter().zip(&values) {
                    if self.better(candidate, score) {
                        best = value;
                        score = candidate.loss;
                    }
                }
                if existing.is_none() && baseline - score < 0.5 {
                    self.cfg.core[core].workload.remove(&workload);
                } else {
                    self.cfg.core[core].workload.insert(workload, best);
                }
                self.rebuild();
            }
        }
    }

    /// Moves every limit above the deepest offset by one common shift, with the past-limit rate scaled so the
    /// hazard past each limit is unchanged.
    pub(crate) fn fit_limit_rate_shift(&mut self, all: &[usize]) {
        let shifts = limit_slots(&self.cfg);
        if shifts.is_empty() || self.cfg.model.past_limit_rate == 0.0 {
            return;
        }
        let (mut low, mut high) = (-50, 50);
        for (_, value) in &shifts {
            low = low.max(-50 - value);
            high = high.min(1 - value);
        }
        let rate = self.cfg.model.past_limit_rate;
        let growth = self.cfg.model.growth;
        let mut options: Vec<(i64, f64)> = Vec::new();
        for delta in low..=high {
            let exponent = f64::from(i32::try_from(-delta).unwrap_or(0));
            let shifted = rate * growth.powf(exponent);
            if (1e-8..=0.5).contains(&shifted) {
                options.push((delta, shifted));
            }
        }
        let apply = |def: &mut MachineDefinition, &(delta, shifted): &(i64, f64)| {
            for (slot, value) in &shifts {
                slot.write(def, value + delta);
            }
            def.model.past_limit_rate = shifted;
        };
        let mut best = (0, rate);
        let mut score = self.score(all);
        let candidates = self.scan(&options, apply, all);
        for (candidate, option) in candidates.iter().zip(&options) {
            if self.better(candidate, score) {
                best = *option;
                score = candidate.loss;
            }
        }
        apply(&mut self.cfg, &best);
        self.rebuild();
    }
}

/// A limit that the shared shift moves.
enum Slot {
    Alone(usize, usize),
    Together(usize, usize),
    Idle(usize),
    Workload(usize, WorkloadId),
}

impl Slot {
    fn write(&self, def: &mut MachineDefinition, value: i64) {
        match self {
            Self::Alone(core, r) => def.core[*core].alone[*r] = value,
            Self::Together(core, r) => def.core[*core].together[*r] = value,
            Self::Idle(core) => def.core[*core].idle = Some(value),
            Self::Workload(core, key) => {
                def.core[*core].workload.insert(*key, value);
            }
        }
    }
}

/// Every limit above the deepest offset, with its value, in a fixed order.
fn limit_slots(cfg: &MachineDefinition) -> Vec<(Slot, i64)> {
    let mut slots = Vec::new();
    for (core, limits) in cfg.core.iter().enumerate() {
        for (r, &value) in limits.alone.iter().enumerate() {
            if value > DEEPEST {
                slots.push((Slot::Alone(core, r), value));
            }
        }
        for (r, &value) in limits.together.iter().enumerate() {
            if value > DEEPEST {
                slots.push((Slot::Together(core, r), value));
            }
        }
        if let Some(value) = limits.idle.filter(|&value| value > DEEPEST) {
            slots.push((Slot::Idle(core), value));
        }
        for (key, &value) in &limits.workload {
            if value > DEEPEST {
                slots.push((Slot::Workload(core, *key), value));
            }
        }
    }
    slots
}
