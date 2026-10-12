//! The coordinate search: each step fits one group of parameters with the others held.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use rayon::prelude::*;
use togi_machine::{REGIMES, Regime};
use togi_sim::definition::{CcdDef, JointDef};
use togi_sim::{MachineDefinition, Model};
use togi_tool_modelcheck::Checker;
use togi_tool_trialfacts::TrialFact;

use crate::error::Error;
use crate::evidence::{DEEPEST, aggregate, initial_config, is_failure, regime_of};
use crate::likelihood::Likelihood;
use crate::signals::fit_signals;

const SWEEPS: usize = 12;
const CONVERGED: f64 = 1e-5;
const MAX_JOINTS_PER_CCD: usize = 8;
const MIN_WORKLOAD_TRIALS: u32 = 10;
const LIMIT_RANGE: (i64, i64) = (DEEPEST, 1);

/// Fits the records and returns the machine with its negative log likelihood.
pub(crate) fn fit(records: &[TrialFact]) -> Result<(MachineDefinition, f64), Error> {
    fit_from(records, None, None)
}

/// [`fit`] starting from `initial` rather than the default start, and, with a `guard`, keeping only models
/// the model check admits.
pub(crate) fn fit_from(
    records: &[TrialFact],
    initial: Option<&MachineDefinition>,
    guard: Option<&Checker>,
) -> Result<(MachineDefinition, f64), Error> {
    let cfg = initial.map_or_else(|| initial_config(records), Clone::clone);
    let obs = aggregate(records);
    let mut l = Likelihood::new(cfg, &obs, guard, None);
    let all = l.all();
    let mut previous = f64::INFINITY;
    for _ in 0..SWEEPS {
        if l.cfg.ccd.is_none() {
            l.fit_joints();
            for ccd in 0..2 {
                l.add_joint(records, ccd);
            }
        } else {
            l.fit_ccd();
        }
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
    if l.cfg.ccd.is_none() {
        l.cfg.ccd = Some(CcdDef {
            log_rate: -9.0,
            slope: 0.1,
            effect: [0.0; 2],
        });
        l.rebuild();
        l.fit_ccd();
    }
    fit_signals(&mut l.cfg, records);
    Model::new(&l.cfg, 0)?;
    let loss = l.score(&all);
    Ok((l.cfg, loss))
}

fn with_ccd(def: &mut MachineDefinition, change: impl FnOnce(&mut CcdDef)) {
    if let Some(ccd) = def.ccd.as_mut() {
        change(ccd);
    }
}

impl Likelihood<'_> {
    pub(crate) fn fit_ccd(&mut self) {
        let indices = self.r7();
        let floor = (-5.0_f64).exp();
        for _ in 0..SWEEPS {
            let before = self.score(&indices);
            self.continuous(
                &indices,
                |d| d.ccd.as_ref().map_or(0.0, |c| c.log_rate.exp()),
                |d, x| with_ccd(d, |c| c.log_rate = x.max(1e-12).ln()),
                1e-7,
                0.1,
            );
            self.continuous(
                &indices,
                |d| d.ccd.as_ref().map_or(0.0, |c| c.slope),
                |d, x| with_ccd(d, |c| c.slope = x),
                1e-4,
                0.5,
            );
            for ccd in 0..2 {
                self.continuous(
                    &indices,
                    |d| d.ccd.as_ref().map_or(0.0, |c| c.effect[ccd].exp()),
                    |d, x| with_ccd(d, |c| c.effect[ccd] = x.max(floor).ln()),
                    floor,
                    floor.recip(),
                );
            }
            if before - self.score(&indices) < CONVERGED {
                break;
            }
        }
    }

    pub(crate) fn fit_joints(&mut self) {
        for j in 0..self.cfg.joint.len() {
            let regimes = self.cfg.joint[j].regimes.clone();
            let indices = self.select(|o| regimes.contains(&o.regime()));
            self.continuous(
                &indices,
                |d| d.joint[j].rate.unwrap_or(0.0),
                |d, x| d.joint[j].rate = Some(x.max(1e-12)),
                1e-7,
                0.5,
            );
            let members: Vec<i64> = self.cfg.joint[j].members.keys().copied().collect();
            for core in members {
                self.discrete(
                    |d| d.joint[j].members.entry(core).or_insert(DEEPEST),
                    &indices,
                    DEEPEST,
                    0,
                );
            }
        }
    }

    pub(crate) fn fit_regime_limits(&mut self) {
        let ccd = self.cfg.ccd.is_some();
        for (core, id) in self.core_ids().into_iter().enumerate() {
            for (r, regime) in REGIMES.into_iter().enumerate() {
                if ccd && regime == Regime::R7 {
                    continue;
                }
                for alone in [true, false] {
                    if alone && r >= 5 {
                        continue;
                    }
                    let indices = self.select(|o| {
                        if o.regime() != regime || !o.loads(id) {
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

    pub(crate) fn fit_flat(&mut self) {
        for core in 0..self.cores() {
            let indices = self.select(|o| o.offset(core) < 0);
            self.continuous(
                &indices,
                |d| d.core[core].flat,
                |d, x| d.core[core].flat = x,
                1e-10,
                0.001,
            );
        }
    }

    pub(crate) fn fit_idle(&mut self) {
        for (core, id) in self.core_ids().into_iter().enumerate() {
            let indices = self.select(|o| !o.loads(id));
            let exposed = indices.iter().any(|&i| self.obs[i].offset(core) < 0);
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

    /// Adds, to CCD `ccd`, the failure-profile joint that most improves the R7 likelihood, if one improves
    /// it by at least 0.5.
    pub(crate) fn add_joint(&mut self, records: &[TrialFact], ccd: usize) {
        let cores = self.cores();
        let half = cores / 2;
        let first = ccd * half;
        let count = self
            .cfg
            .joint
            .iter()
            .filter(|joint| {
                i64::try_from(first).is_ok_and(|first| joint.members.contains_key(&first))
            })
            .count();
        if count >= MAX_JOINTS_PER_CCD {
            return;
        }
        let indices = self.r7();
        let baseline = self.score(&indices);
        let mut best_score = baseline - 0.5;
        let original = self.cfg.joint.len();
        let mut seen: HashSet<BTreeMap<i64, i64>> = HashSet::new();
        let mut candidates: Vec<BTreeMap<i64, i64>> = Vec::new();
        for fact in records {
            if regime_of(fact) != Regime::R7 || !is_failure(fact) {
                continue;
            }
            let mut members = BTreeMap::new();
            for (core, offset) in fact
                .profile
                .as_slice()
                .iter()
                .enumerate()
                .skip(first)
                .take(half)
            {
                if offset.get() == 0 {
                    break;
                }
                members.insert(i64::try_from(core).unwrap_or(i64::MAX), offset.get());
            }
            if members.len() != half || !seen.insert(members.clone()) {
                continue;
            }
            if self.cfg.joint.iter().any(|joint| joint.members == members) {
                continue;
            }
            candidates.push(members);
        }
        let results: Vec<(JointDef, f64)> = candidates
            .into_par_iter()
            .map(|members| {
                let mut cfg = self.cfg.clone();
                cfg.joint.push(JointDef {
                    members,
                    regimes: vec![Regime::R7],
                    rate: Some(0.001),
                    ..JointDef::default()
                });
                let mut trial = Likelihood::new(cfg, self.obs, self.guard, self.named);
                trial.continuous(
                    &indices,
                    |d| d.joint[original].rate.unwrap_or(0.0),
                    |d, x| d.joint[original].rate = Some(x.max(1e-12)),
                    1e-7,
                    0.5,
                );
                let score = trial.score(&indices);
                (trial.cfg.joint.swap_remove(original), score)
            })
            .collect();
        let mut best = None;
        for (joint, score) in results {
            if score < best_score {
                best_score = score;
                best = Some(joint);
            }
        }
        if let Some(joint) = best {
            self.cfg.joint.push(joint);
        }
        self.rebuild();
    }

    pub(crate) fn fit_workloads(&mut self) {
        let ccd = self.cfg.ccd.is_some();
        for (core, id) in self.core_ids().into_iter().enumerate() {
            let workloads: BTreeSet<&'static str> = self
                .obs
                .iter()
                .filter(|o| !(ccd && o.regime() == Regime::R7) && o.k > 0 && o.loads(id))
                .map(|o| o.class.workload.as_str())
                .collect();
            for workload in workloads {
                let indices = self.select(|o| o.class.workload.as_str() == workload && o.loads(id));
                let trials: u32 = indices.iter().map(|&i| self.obs[i].n).sum();
                if trials < MIN_WORKLOAD_TRIALS {
                    continue;
                }
                let baseline = self.score(&indices);
                let existing = self.cfg.core[core].workload.get(workload).copied();
                let mut best = existing.unwrap_or(DEEPEST);
                let mut score = f64::INFINITY;
                let values: Vec<i64> = (LIMIT_RANGE.0..=LIMIT_RANGE.1).collect();
                let candidates = self.scan(
                    &values,
                    |d, &value| {
                        d.core[core].workload.insert(workload.to_owned(), value);
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
                    self.cfg.core[core].workload.remove(workload);
                } else {
                    self.cfg.core[core]
                        .workload
                        .insert(workload.to_owned(), best);
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
    Workload(usize, String),
}

impl Slot {
    fn write(&self, def: &mut MachineDefinition, value: i64) {
        match self {
            Self::Alone(core, r) => def.core[*core].alone[*r] = value,
            Self::Together(core, r) => def.core[*core].together[*r] = value,
            Self::Idle(core) => def.core[*core].idle = Some(value),
            Self::Workload(core, key) => {
                def.core[*core].workload.insert(key.clone(), value);
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
                slots.push((Slot::Workload(core, key.clone()), value));
            }
        }
    }
    slots
}
