//! The objective of a fit and the two searches every coordinate step is built from.
//!
//! A [`Likelihood`] holds the machine being fitted as a [`MachineDefinition`] and, built from it, the
//! [`Model`] that predicts. A candidate that the model refuses is inadmissible: its loss is infinite, so a
//! search never keeps it.
//!
//! Candidates of a [`scan`](Likelihood::scan) are built and scored on rayon's threads, each into its own
//! slot of the result, and every score sums its observations in index order on one thread. The caller then
//! folds the slots in candidate order with a strict improvement test, so the winner never depends on how
//! many threads ran or which finished first.

use rayon::prelude::*;
use togi_machine::{CoreId, Regime};
use togi_sim::{MachineDefinition, Model};
use togi_tool_modelcheck::Checker;

use crate::evidence::{Observation, load_of};
use crate::voltage::{NamedFailure, named_loss, voltage_penalty};

/// A candidate must beat the incumbent by more than this to replace it.
pub(crate) const IMPROVEMENT: f64 = 1e-9;
const GOLDEN: f64 = 0.618_033_988_749_894_9;

pub(crate) struct Candidate {
    pub model: Option<Model>,
    pub loss: f64,
}

pub(crate) struct Likelihood<'a> {
    pub cfg: MachineDefinition,
    pub model: Option<Model>,
    pub obs: &'a [Observation],
    /// When set, a model is admissible only if it passes the model check against the original evidence.
    pub guard: Option<&'a Checker>,
    /// When set, the objective also holds the shared-voltage prior and the named-core terms.
    pub named: Option<&'a [NamedFailure]>,
}

impl<'a> Likelihood<'a> {
    pub(crate) fn new(
        cfg: MachineDefinition,
        obs: &'a [Observation],
        guard: Option<&'a Checker>,
        named: Option<&'a [NamedFailure]>,
    ) -> Self {
        let mut likelihood = Self {
            cfg,
            model: None,
            obs,
            guard,
            named,
        };
        likelihood.rebuild();
        likelihood
    }

    pub(crate) fn rebuild(&mut self) {
        self.model = Model::new(&self.cfg, 0).ok();
    }

    pub(crate) fn cores(&self) -> usize {
        usize::try_from(self.cfg.cores).unwrap_or(0)
    }

    pub(crate) fn core_ids(&self) -> Vec<CoreId> {
        (0..self.cores())
            .filter_map(|core| CoreId::try_from(core).ok())
            .collect()
    }

    pub(crate) fn select(&self, include: impl Fn(&Observation) -> bool) -> Vec<usize> {
        self.obs
            .iter()
            .enumerate()
            .filter(|(_, observation)| include(observation))
            .map(|(i, _)| i)
            .collect()
    }

    pub(crate) fn all(&self) -> Vec<usize> {
        (0..self.obs.len()).collect()
    }

    pub(crate) fn r7(&self) -> Vec<usize> {
        self.select(|o| o.regime() == Regime::R7)
    }

    pub(crate) fn value_of(&self, model: &Model, i: usize) -> f64 {
        let o = &self.obs[i];
        let Ok(p) = model.failure_probability(o.profile.as_slice(), &load_of(&o.class)) else {
            return f64::INFINITY;
        };
        let mut loss = 0.0;
        if o.k > 0 {
            if p <= 0.0 {
                return f64::INFINITY;
            }
            loss -= f64::from(o.k) * p.ln();
        }
        if o.n > o.k {
            if p >= 1.0 {
                return f64::INFINITY;
            }
            loss -= f64::from(o.n - o.k) * (-p).ln_1p();
        }
        loss
    }

    /// The negative log likelihood of `indices` under `model`, with the CCD shrinkage of `def`.
    pub(crate) fn raw_score_on(
        &self,
        def: &MachineDefinition,
        model: &Model,
        indices: &[usize],
    ) -> f64 {
        let mut loss = 0.0;
        for &i in indices {
            loss += self.value_of(model, i);
        }
        if let Some(ccd) = &def.ccd {
            loss += 0.5 * ccd.effect.iter().map(|effect| effect * effect).sum::<f64>();
        }
        loss
    }

    /// What a search minimizes for the machine `def` that `model` was built from.
    fn objective_on(&self, def: &MachineDefinition, model: &Model, indices: &[usize]) -> f64 {
        let raw = self.raw_score_on(def, model, indices);
        let Some(named) = self.named else {
            return raw;
        };
        let Some(voltage) = &def.shared_voltage else {
            return raw;
        };
        raw + voltage_penalty(voltage) + named_loss(model, named)
    }

    pub(crate) fn admits(&self, model: &Model) -> bool {
        self.guard
            .is_none_or(|checker| checker.accepts(model).unwrap_or(false))
    }

    /// The reported likelihood of `indices`: the objective without the shared-voltage terms.
    pub(crate) fn raw_score(&self, indices: &[usize]) -> f64 {
        self.model.as_ref().map_or(f64::INFINITY, |model| {
            self.raw_score_on(&self.cfg, model, indices)
        })
    }

    pub(crate) fn score(&self, indices: &[usize]) -> f64 {
        match &self.model {
            Some(model) if self.admits(model) => self.objective_on(&self.cfg, model, indices),
            _ => f64::INFINITY,
        }
    }

    /// Builds and scores one candidate machine per item: `apply` edits a copy of the current machine. The
    /// result has one slot per item, in item order. `apply` must not change the CCD or shared-voltage terms.
    pub(crate) fn scan<T: Sync>(
        &self,
        items: &[T],
        apply: impl Fn(&mut MachineDefinition, &T) + Sync,
        indices: &[usize],
    ) -> Vec<Candidate> {
        items
            .par_iter()
            .map(|item| {
                let mut def = self.cfg.clone();
                apply(&mut def, item);
                let model = Model::new(&def, 0).ok();
                let loss = model.as_ref().map_or(f64::INFINITY, |model| {
                    self.objective_on(&def, model, indices)
                });
                Candidate { model, loss }
            })
            .collect()
    }

    /// Whether a candidate improves on `score` and the guard admits it.
    pub(crate) fn better(&self, candidate: &Candidate, score: f64) -> bool {
        candidate.loss < score - IMPROVEMENT
            && candidate
                .model
                .as_ref()
                .is_some_and(|model| self.admits(model))
    }

    /// Tries every integer limit in `first..=last` at the slot `locate` names and keeps the best.
    pub(crate) fn discrete(
        &mut self,
        locate: impl for<'x> Fn(&'x mut MachineDefinition) -> &'x mut i64 + Sync,
        indices: &[usize],
        first: i64,
        last: i64,
    ) {
        if indices.is_empty() {
            return;
        }
        let mut best = *locate(&mut self.cfg);
        let mut score = self.score(indices);
        let values: Vec<i64> = (first..=last).collect();
        let candidates = self.scan(&values, |def, &value| *locate(def) = value, indices);
        for (candidate, &value) in candidates.iter().zip(&values) {
            if self.better(candidate, score) {
                best = value;
                score = candidate.loss;
            }
        }
        *locate(&mut self.cfg) = best;
        self.rebuild();
    }

    /// Golden-section search over a parameter's value between `low` and `high`, in log space, after one probe
    /// at zero.
    pub(crate) fn continuous(
        &mut self,
        indices: &[usize],
        get: impl Fn(&MachineDefinition) -> f64,
        set: impl Fn(&mut MachineDefinition, f64),
        low: f64,
        high: f64,
    ) {
        if indices.is_empty() {
            return;
        }
        let start = get(&self.cfg);
        self.minimize(
            indices,
            start,
            &set,
            (low.ln(), high.ln()),
            24,
            f64::exp,
            true,
        );
    }

    /// Golden-section search over the parameter itself between `low` and `high`.
    pub(crate) fn linear(
        &mut self,
        indices: &[usize],
        start: f64,
        set: impl Fn(&mut MachineDefinition, f64),
        low: f64,
        high: f64,
    ) {
        if indices.is_empty() {
            return;
        }
        self.minimize(
            indices,
            start,
            &set,
            (low, high),
            20,
            std::convert::identity,
            false,
        );
    }

    fn evaluate(
        &mut self,
        indices: &[usize],
        set: &impl Fn(&mut MachineDefinition, f64),
        value: f64,
        best: &mut (f64, f64),
    ) -> f64 {
        set(&mut self.cfg, value);
        self.rebuild();
        let loss = self.score(indices);
        if loss < best.1 - IMPROVEMENT {
            *best = (value, loss);
        }
        loss
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one search body serves the log-space and the linear callers"
    )]
    fn minimize(
        &mut self,
        indices: &[usize],
        start: f64,
        set: &impl Fn(&mut MachineDefinition, f64),
        bracket: (f64, f64),
        rounds: usize,
        to_value: fn(f64) -> f64,
        probe_zero: bool,
    ) {
        let mut best = (start, self.score(indices));
        if probe_zero {
            self.evaluate(indices, set, 0.0, &mut best);
        }
        let (mut a, mut b) = bracket;
        let mut x = b - GOLDEN * (b - a);
        let mut y = a + GOLDEN * (b - a);
        let mut fx = self.evaluate(indices, set, to_value(x), &mut best);
        let mut fy = self.evaluate(indices, set, to_value(y), &mut best);
        for _ in 0..rounds {
            if fx < fy {
                b = y;
                y = x;
                fy = fx;
                x = b - GOLDEN * (b - a);
                fx = self.evaluate(indices, set, to_value(x), &mut best);
            } else {
                a = x;
                x = y;
                fx = fy;
                y = a + GOLDEN * (b - a);
                fy = self.evaluate(indices, set, to_value(y), &mut best);
            }
        }
        set(&mut self.cfg, best.0);
        self.rebuild();
    }
}
