//! Exact depth reference for the successor truth.
//!
//! A [`Reference`] integrates, once, the per-core hazard of every physical depth `0..=50` over a
//! periodic calendar at the exact thermal fixed point. Everything afterwards is table lookup:
//! [`Reference::admissibility`] judges any (possibly nonuniform) profile, and
//! [`Reference::depth_reference`] certifies the deepest profile that is uniform within each
//! tuning unit.
//!
//! # Certificate rationale
//!
//! Every per-core rate table and the pair-root table is checked to be finite, nonnegative and
//! nondecreasing in depth. Under that check the total hazard of a unit-uniform profile
//! `D = (D_u)` is
//!
//! ```text
//! T(D) = sum_u R_u(D_u) + sum_pairs fraction * root(D_a) * root(D_b)
//! ```
//!
//! which is nondecreasing in every `D_u`. The admissible set `{R_u(D_u) <= lambda/n, T(D) <=
//! lambda}` is therefore a down-set of the depth lattice, and the per-unit limit is a prefix
//! `D_u <= m_u`. A profile of total depth `s` exists iff one of minimal cost does, so the exact
//! optimum is the largest `s` whose *minimum* cost over all assignments fits the budget.
//!
//! That minimum is computed exactly, not sampled. Units that share no pair are separable: their
//! cost tables (`min cost per total depth`) combine by min-plus convolution. Units coupled by a
//! sparse pair term form a connected component whose table is built by exhaustive depth-first
//! enumeration, pruned only when a partial assignment already exceeds the budget (sound, because
//! costs never decrease). Pair terms inside one unit fold into that unit's own cost. No
//! heuristic cutoff or approximation enters: [`Certificate::upper_bound`] is the exact table
//! maximum, and [`Certificate::value`] is a witness profile re-judged through
//! [`Reference::admissibility`].

use std::collections::BTreeSet;
use std::time::Duration;

use togi_machine::{CoreId, CoreInfo, PerCore};

use super::exposure::periodic;
use super::{Depth, InteractionId, OPERATIONAL_DEPTH, Profile, Scenario, TruthError, number};

/// Deepest runtime-writable physical depth.
const PHYSICAL_DEPTH: u8 = 50;
const LEVELS: usize = 51;
/// Physical topology is bounded to sixteen cores by `CoreId`.
const CORES: usize = 16;
const CYCLE: Duration = Duration::from_hours(24);

type Table = [f64; LEVELS];

fn depth(value: usize) -> Result<Depth, TruthError> {
    Depth::new(u8::try_from(value).map_err(|_| TruthError::Invalid("physical-core depth"))?)
}

/// Explicit partition of the machine's physical cores into tuning units.
///
/// Every topology core is in exactly one non-empty group. The grouping does not restrict
/// [`Reference::admissibility`]; it only defines which cores the depth optimization holds equal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnitGrouping {
    groups: Vec<Vec<CoreId>>,
}

impl UnitGrouping {
    /// Validate an explicit grouping as an exact partition of `topology`.
    pub fn new(topology: &[CoreInfo], groups: Vec<Vec<CoreId>>) -> Result<Self, TruthError> {
        let grouping = Self { groups };
        grouping.check(topology)?;
        Ok(grouping)
    }

    /// One unit per CCD, in ascending CCD order, cores ascending within each unit.
    pub fn ccd(topology: &[CoreInfo]) -> Result<Self, TruthError> {
        let ccds = topology
            .iter()
            .map(|info| info.ccd.index())
            .collect::<BTreeSet<_>>();
        let groups = ccds
            .into_iter()
            .map(|ccd| {
                let mut cores = topology
                    .iter()
                    .filter(|info| info.ccd.index() == ccd)
                    .map(|info| info.core)
                    .collect::<Vec<_>>();
                cores.sort_unstable();
                cores
            })
            .collect();
        Self::new(topology, groups)
    }

    /// One unit per physical core, in ascending core order.
    pub fn cores(topology: &[CoreInfo]) -> Result<Self, TruthError> {
        let mut cores = topology.iter().map(|info| info.core).collect::<Vec<_>>();
        cores.sort_unstable();
        Self::new(topology, cores.into_iter().map(|core| vec![core]).collect())
    }

    pub fn groups(&self) -> &[Vec<CoreId>] {
        &self.groups
    }

    fn check(&self, topology: &[CoreInfo]) -> Result<(), TruthError> {
        let expected = topology
            .iter()
            .map(|info| info.core)
            .collect::<BTreeSet<_>>();
        let mut seen = BTreeSet::new();
        for group in &self.groups {
            if group.is_empty() {
                return Err(TruthError::Invalid("empty tuning unit"));
            }
            for core in group {
                if !expected.contains(core) || !seen.insert(*core) {
                    return Err(TruthError::Invalid("tuning units are not a partition"));
                }
            }
        }
        if expected.is_empty() || seen != expected {
            return Err(TruthError::Invalid(
                "tuning units do not cover the topology",
            ));
        }
        Ok(())
    }
}

/// Ordered daily active mix and the sleep that completes the 24-hour cycle.
#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceCalendar {
    active: Duration,
    fractions: Vec<f64>,
}

impl ReferenceCalendar {
    /// `fractions` follow the scenario's `daily_mix` order and sum to one; the remainder of
    /// 24 hours after `active` is asleep.
    pub fn new(active: Duration, fractions: Vec<f64>) -> Result<Self, TruthError> {
        if active.is_zero() || active > CYCLE || fractions.is_empty() {
            return Err(TruthError::Invalid("daily reference calendar"));
        }
        if fractions
            .iter()
            .any(|fraction| !fraction.is_finite() || *fraction < 0.0)
        {
            return Err(TruthError::Invalid("daily reference fractions"));
        }
        let sum = fractions.iter().sum::<f64>();
        if !sum.is_finite() || (sum - 1.0).abs() > 1e-12 {
            return Err(TruthError::Invalid("daily reference fractions"));
        }
        Ok(Self { active, fractions })
    }

    /// Eight active hours in the scenario's own daily-mix proportions.
    pub fn standard(scenario: &Scenario) -> Result<Self, TruthError> {
        Self::new(
            Duration::from_hours(8),
            scenario
                .daily_mix()
                .iter()
                .map(|block| block.fraction)
                .collect(),
        )
    }

    pub const fn active(&self) -> Duration {
        self.active
    }

    pub fn fractions(&self) -> &[f64] {
        &self.fractions
    }

    /// Per-block durations; the last block takes the remainder so they sum to `active`.
    fn durations(&self) -> Vec<Duration> {
        let sum = self.fractions.iter().sum::<f64>();
        let mut remaining = self.active;
        let last = self.fractions.len() - 1;
        self.fractions
            .iter()
            .enumerate()
            .map(|(index, fraction)| {
                let duration = if index == last {
                    remaining
                } else {
                    self.active.mul_f64(fraction / sum).min(remaining)
                };
                remaining = remaining.saturating_sub(duration);
                duration
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Pair {
    a: CoreId,
    b: CoreId,
    /// Share of active time during which both cores are loaded.
    fraction: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Cross {
    a: usize,
    b: usize,
    fraction: f64,
}

/// One stable interaction source's CO-added reference rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InteractionRate {
    pub source: InteractionId,
    pub per_hour: f64,
}

/// Verdict for one profile against the per-unit and joint hazard limits.
///
/// `joint` is the whole machine's competing CO hazard, per-core terms plus each pair term once,
/// with stock background excluded. Ratios are rate over limit; `member` allows `tolerance`
/// absolute failures per active hour above each limit.
#[derive(Clone, Debug, PartialEq)]
pub struct Admissibility {
    pub per_unit: Vec<f64>,
    pub joint: f64,
    pub interactions: Vec<InteractionRate>,
    pub per_unit_ratios: Vec<f64>,
    pub joint_ratio: f64,
    pub member: bool,
    pub tolerance: f64,
}

/// Proof that `value` is the exact optimum of one capped depth objective.
///
/// `value` is the total depth of a witness profile that [`Reference::admissibility`] accepts;
/// `upper_bound` is the largest total depth whose exact minimum-cost assignment fits the hazard
/// budget. `value == upper_bound` therefore proves optimality over every unit-uniform profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Certificate {
    pub value: u32,
    pub upper_bound: u32,
    /// Depth cap applied to every unit.
    pub cap: u8,
    /// Partial assignments enumerated inside coupled components.
    pub states: u64,
    /// Independent unit groups that were combined by min-plus convolution.
    pub components: usize,
}

/// Exact optima of the sum of unit depths under the operational envelope and physical range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepthReference {
    /// Every unit capped at the operational depth.
    pub operational: Certificate,
    /// Every unit capped at the physical maximum of 50.
    pub physical: Certificate,
    /// What the operational envelope forgoes: `physical.value - operational.value`.
    pub physical_gap: u32,
    /// Deepest depth each unit reaches alone with every other unit at depth 0, within the
    /// operational envelope.
    pub unit_maxima: Vec<u8>,
}

/// Owned, precomputed truth for one scenario, day, grouping and calendar.
#[derive(Clone, Debug)]
pub struct Reference {
    grouping: UnitGrouping,
    rates: [Option<Table>; CORES],
    core_count: usize,
    pairs: Vec<Pair>,
    roots: Table,
    /// Per-unit summed core rates, the quantity held under `lambda / units`.
    unit_rate: Vec<Table>,
    /// `unit_rate` plus the pair terms whose two cores share the unit.
    unit_cost: Vec<Table>,
    cross: Vec<Cross>,
    lambda: f64,
    tolerance: f64,
}

impl Reference {
    /// Integrate the scenario's day (one-based) over `calendar` at the periodic thermal state.
    pub fn new(
        scenario: &Scenario,
        day: u32,
        grouping: UnitGrouping,
        calendar: ReferenceCalendar,
        lambda: f64,
        tolerance: f64,
    ) -> Result<Self, TruthError> {
        if day == 0 {
            return Err(TruthError::Invalid("one-based calendar day"));
        }
        let topology = scenario.topology();
        grouping.check(topology)?;
        let blocks = scenario.daily_mix();
        if calendar.fractions.len() != blocks.len() {
            return Err(TruthError::Invalid("daily reference calendar"));
        }
        let durations = calendar.durations();
        let active_s = calendar.active.as_secs_f64();
        let sleep = CYCLE - calendar.active;
        let mut cycle = blocks
            .iter()
            .zip(&durations)
            .filter(|(_, duration)| !duration.is_zero())
            .map(|(block, duration)| (Some(&block.class), *duration))
            .collect::<Vec<_>>();
        if !sleep.is_zero() {
            cycle.push((None, sleep));
        }
        let mut state = periodic(topology, &cycle)?;

        let mut rates = [None; CORES];
        for info in topology {
            rates[info.core.index()] = Some([0.0; LEVELS]);
        }
        for (block, &duration) in blocks.iter().zip(&durations) {
            if duration.is_zero() {
                continue;
            }
            let loaded = block.class.loaded();
            for info in topology {
                if !loaded.contains(info.core) {
                    continue;
                }
                let exposed = duration.as_secs_f64()
                    - scenario.after_seconds(&block.class, &state, info.core)?;
                let weight = exposed.max(0.0) / active_s;
                if weight == 0.0 {
                    continue;
                }
                let table = rates[info.core.index()]
                    .as_mut()
                    .ok_or(TruthError::Invalid("core topology"))?;
                for (level, rate) in table.iter_mut().enumerate() {
                    *rate +=
                        weight * scenario.core_rate(&block.class, info.core, depth(level)?, day)?;
                }
            }
            state.advance(Some(&block.class), duration)?;
        }

        let mut pairs = Vec::new();
        for (a, b) in scenario.joint_pairs().iter().copied() {
            let fraction = blocks
                .iter()
                .zip(&durations)
                .filter(|(block, _)| {
                    let loaded = block.class.loaded();
                    loaded.contains(a) && loaded.contains(b)
                })
                .map(|(_, duration)| duration.as_secs_f64() / active_s)
                .sum();
            pairs.push(Pair { a, b, fraction });
        }
        let mut roots = [0.0; LEVELS];
        if !pairs.is_empty() {
            for (level, root) in roots.iter_mut().enumerate() {
                *root = scenario.joint_root(depth(level)?)?;
            }
        }
        Self::assemble(grouping, rates, pairs, roots, lambda, tolerance)
    }

    /// Validate monotone tables and derive the per-unit and cross-unit structure.
    fn assemble(
        grouping: UnitGrouping,
        rates: [Option<Table>; CORES],
        pairs: Vec<Pair>,
        roots: Table,
        lambda: f64,
        tolerance: f64,
    ) -> Result<Self, TruthError> {
        if !lambda.is_finite() || lambda <= 0.0 {
            return Err(TruthError::Invalid("lambda per hour"));
        }
        if !tolerance.is_finite() || tolerance < 0.0 {
            return Err(TruthError::Invalid("reference tolerance"));
        }
        let monotone = |table: &Table| {
            table.iter().all(|value| value.is_finite() && *value >= 0.0)
                && table.windows(2).all(|pair| pair[0] <= pair[1])
        };
        if !monotone(&roots) || rates.iter().flatten().any(|table| !monotone(table)) {
            return Err(TruthError::Invalid(
                "reference hazard is not finite, nonnegative and monotone",
            ));
        }
        let mut unit_of = [None; CORES];
        let mut core_count = 0;
        for (unit, group) in grouping.groups.iter().enumerate() {
            for core in group {
                if rates[core.index()].is_none() {
                    return Err(TruthError::Invalid("core topology"));
                }
                unit_of[core.index()] = Some(unit);
                core_count += 1;
            }
        }
        if rates.iter().flatten().count() != core_count {
            return Err(TruthError::Invalid(
                "tuning units do not cover the topology",
            ));
        }
        let mut unit_rate = Vec::with_capacity(grouping.groups.len());
        for group in &grouping.groups {
            let mut table = [0.0; LEVELS];
            for core in group {
                let core_table = rates[core.index()]
                    .as_ref()
                    .ok_or(TruthError::Invalid("core topology"))?;
                for (sum, rate) in table.iter_mut().zip(core_table) {
                    *sum += rate;
                }
            }
            unit_rate.push(table);
        }
        let mut unit_cost = unit_rate.clone();
        let mut cross = Vec::new();
        for pair in &pairs {
            if !pair.fraction.is_finite() || pair.fraction < 0.0 {
                return Err(TruthError::Invalid("joint fraction"));
            }
            let (Some(a), Some(b)) = (unit_of[pair.a.index()], unit_of[pair.b.index()]) else {
                return Err(TruthError::Invalid("joint pair topology"));
            };
            if a == b {
                for (cost, root) in unit_cost[a].iter_mut().zip(roots) {
                    *cost += pair.fraction * root * root;
                }
            } else {
                cross.push(Cross {
                    a,
                    b,
                    fraction: pair.fraction,
                });
            }
        }
        Ok(Self {
            grouping,
            rates,
            core_count,
            pairs,
            roots,
            unit_rate,
            unit_cost,
            cross,
            lambda,
            tolerance,
        })
    }

    pub fn grouping(&self) -> &UnitGrouping {
        &self.grouping
    }

    /// Judge any complete profile; grouping does not impose uniformity on the supplied vector.
    pub fn admissibility(&self, profile: &Profile) -> Result<Admissibility, TruthError> {
        let depths = profile.depths();
        if depths.len() != self.core_count {
            return Err(TruthError::Invalid("profile topology"));
        }
        let lookup = |core: CoreId| {
            depths
                .get(core)
                .map(|depth| usize::from(depth.get()))
                .ok_or(TruthError::Invalid("profile topology"))
        };
        let mut per_unit = Vec::with_capacity(self.grouping.groups.len());
        for group in &self.grouping.groups {
            let mut sum = 0.0;
            for core in group {
                let table = self.rates[core.index()]
                    .as_ref()
                    .ok_or(TruthError::Invalid("core topology"))?;
                sum += table[lookup(*core)?];
            }
            per_unit.push(sum);
        }
        let interactions = self
            .pairs
            .iter()
            .map(|pair| {
                Ok(InteractionRate {
                    source: InteractionId::new(pair.a, pair.b)?,
                    per_hour: pair.fraction
                        * self.roots[lookup(pair.a)?]
                        * self.roots[lookup(pair.b)?],
                })
            })
            .collect::<Result<Vec<_>, TruthError>>()?;
        let joint = per_unit.iter().sum::<f64>()
            + interactions
                .iter()
                .map(|source| source.per_hour)
                .sum::<f64>();
        let share = self.share();
        let per_unit_ratios = per_unit.iter().map(|rate| rate / share).collect();
        let member = per_unit.iter().all(|rate| *rate <= share + self.tolerance)
            && joint <= self.lambda + self.tolerance;
        Ok(Admissibility {
            per_unit,
            interactions,
            joint,
            per_unit_ratios,
            joint_ratio: joint / self.lambda,
            member,
            tolerance: self.tolerance,
        })
    }

    /// Certified exact optima of the summed unit depths, see the module documentation.
    pub fn depth_reference(&self) -> Result<DepthReference, TruthError> {
        let operational = self.certify(OPERATIONAL_DEPTH)?;
        let physical = self.certify(PHYSICAL_DEPTH)?;
        let physical_gap =
            physical
                .value
                .checked_sub(operational.value)
                .ok_or(TruthError::Invalid(
                    "physical optimum below operational optimum",
                ))?;
        let units = self.grouping.groups.len();
        let budget = self.lambda + self.tolerance;
        let mut unit_maxima = Vec::with_capacity(units);
        let mut depths = vec![0; units];
        for unit in 0..units {
            let mut best = 0;
            for level in 0..=usize::from(OPERATIONAL_DEPTH) {
                depths[unit] = level;
                if self.unit_rate[unit][level] <= self.share() + self.tolerance
                    && self.total(&depths) <= budget
                {
                    best = level;
                }
            }
            unit_maxima
                .push(u8::try_from(best).map_err(|_| TruthError::Invalid("physical-core depth"))?);
            depths[unit] = 0;
        }
        Ok(DepthReference {
            operational,
            physical,
            physical_gap,
            unit_maxima,
        })
    }

    fn share(&self) -> f64 {
        self.lambda / number(self.grouping.groups.len())
    }

    /// Total hazard of a unit-uniform profile.
    fn total(&self, depths: &[usize]) -> f64 {
        let mut total = depths
            .iter()
            .enumerate()
            .map(|(unit, level)| self.unit_cost[unit][*level])
            .sum::<f64>();
        for cross in &self.cross {
            total += cross.fraction * self.roots[depths[cross.a]] * self.roots[depths[cross.b]];
        }
        total
    }

    fn profile_for(&self, depths: &[usize]) -> Result<Profile, TruthError> {
        let mut profile = PerCore::default();
        for (group, level) in self.grouping.groups.iter().zip(depths) {
            for core in group {
                profile.set(*core, depth(*level)?);
            }
        }
        Ok(Profile { depths: profile })
    }

    fn certify(&self, cap: u8) -> Result<Certificate, TruthError> {
        let cap = usize::from(cap);
        let units = self.grouping.groups.len();
        let budget = self.lambda + self.tolerance;
        let limit = self.share() + self.tolerance;
        if self.unit_rate.iter().any(|table| table[0] > limit)
            || self.total(&vec![0; units]) > budget
        {
            return Err(TruthError::Invalid("reference has no admissible profile"));
        }
        // The limit is a prefix of depths because the rate tables are monotone.
        let maxima = self
            .unit_rate
            .iter()
            .map(|table| {
                table
                    .iter()
                    .take(cap + 1)
                    .take_while(|rate| **rate <= limit)
                    .count()
                    - 1
            })
            .collect::<Vec<_>>();

        let mut label = (0..units).collect::<Vec<_>>();
        loop {
            let mut changed = false;
            for cross in &self.cross {
                let low = label[cross.a].min(label[cross.b]);
                for unit in [cross.a, cross.b] {
                    if label[unit] != low {
                        label[unit] = low;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let components = (0..units)
            .filter(|unit| label[*unit] == *unit)
            .map(|root| {
                (0..units)
                    .filter(|unit| label[*unit] == root)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let mut states = 0;
        let mut tables = Vec::with_capacity(components.len());
        for component in &components {
            let mut search = Search::new(self, component, &maxima, budget);
            search.descend(0, 0.0, 0);
            states += search.states;
            tables.push(search.table);
        }

        let mut best = vec![Some(0.0)];
        let mut choices = Vec::with_capacity(tables.len());
        for table in &tables {
            let mut next: Vec<Option<f64>> = vec![None; best.len() + table.len() - 1];
            let mut choice = vec![0; next.len()];
            for (base_total, base) in best.iter().enumerate() {
                let Some(base) = base else { continue };
                for (extra, entry) in table.iter().enumerate() {
                    let Some((cost, _)) = entry else { continue };
                    let cost = base + cost;
                    if cost > budget {
                        continue;
                    }
                    let slot = &mut next[base_total + extra];
                    if slot.is_none_or(|old| cost < old) {
                        *slot = Some(cost);
                        choice[base_total + extra] = extra;
                    }
                }
            }
            best = next;
            choices.push(choice);
        }
        let upper_bound = best
            .iter()
            .rposition(Option::is_some)
            .ok_or(TruthError::Invalid("reference has no admissible profile"))?;

        let mut depths = vec![0; units];
        let mut remaining = upper_bound;
        for ((component, table), choice) in components.iter().zip(&tables).zip(&choices).rev() {
            let extra = choice[remaining];
            let (_, assignment) = table[extra]
                .as_ref()
                .ok_or(TruthError::Invalid("reference witness"))?;
            for (unit, level) in component.iter().zip(assignment) {
                depths[*unit] = *level;
            }
            remaining -= extra;
        }
        let witness = self.profile_for(&depths)?;
        if !self.admissibility(&witness)?.member {
            return Err(TruthError::Invalid("reference witness is not admissible"));
        }
        let value = u32::try_from(depths.iter().sum::<usize>())
            .map_err(|_| TruthError::Invalid("reference depth"))?;
        let upper_bound =
            u32::try_from(upper_bound).map_err(|_| TruthError::Invalid("reference depth"))?;
        if value != upper_bound {
            return Err(TruthError::Invalid("reference certificate gap"));
        }
        Ok(Certificate {
            value,
            upper_bound,
            cap: u8::try_from(cap).map_err(|_| TruthError::Invalid("physical-core depth"))?,
            states,
            components: components.len(),
        })
    }
}

/// Exhaustive minimum-cost enumeration inside one coupled component.
struct Search<'a> {
    reference: &'a Reference,
    units: &'a [usize],
    maxima: Vec<usize>,
    /// For each local position, the earlier local positions it shares a pair term with.
    earlier: Vec<Vec<(usize, f64)>>,
    budget: f64,
    depths: Vec<usize>,
    /// Minimum cost and assignment, by total component depth.
    table: Vec<Option<(f64, Vec<usize>)>>,
    states: u64,
}

impl<'a> Search<'a> {
    fn new(reference: &'a Reference, units: &'a [usize], maxima: &[usize], budget: f64) -> Self {
        let mut position = vec![None; reference.unit_cost.len()];
        for (local, unit) in units.iter().enumerate() {
            position[*unit] = Some(local);
        }
        let mut earlier = vec![Vec::new(); units.len()];
        for cross in &reference.cross {
            if let (Some(a), Some(b)) = (position[cross.a], position[cross.b]) {
                earlier[a.max(b)].push((a.min(b), cross.fraction));
            }
        }
        let maxima = units.iter().map(|unit| maxima[*unit]).collect::<Vec<_>>();
        let table = vec![None; maxima.iter().sum::<usize>() + 1];
        Self {
            reference,
            units,
            earlier,
            budget,
            depths: vec![0; maxima.len()],
            maxima,
            table,
            states: 0,
        }
    }

    fn descend(&mut self, position: usize, cost: f64, total: usize) {
        if position == self.units.len() {
            if self.table[total]
                .as_ref()
                .is_none_or(|(best, _)| cost < *best)
            {
                self.table[total] = Some((cost, self.depths.clone()));
            }
            return;
        }
        let unit = self.units[position];
        for level in 0..=self.maxima[position] {
            let mut next = cost + self.reference.unit_cost[unit][level];
            for &(early, fraction) in &self.earlier[position] {
                next += fraction
                    * self.reference.roots[level]
                    * self.reference.roots[self.depths[early]];
            }
            // Costs never decrease with depth, so deeper levels cannot recover.
            if next > self.budget {
                break;
            }
            self.states += 1;
            self.depths[position] = level;
            self.descend(position + 1, next, total + level);
        }
        self.depths[position] = 0;
    }
}

#[cfg(test)]
#[path = "reference_tests.rs"]
mod tests;
