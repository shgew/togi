//! A ruleset's bench gate: the `gate` object of the suite file, registered before the ruleset is scored, and the
//! verdict it gives a run against a recorded baseline.
//!
//! The quantile criteria resample seed pairs with a [`StdRng`] seeded from the gate's
//! `bootstrap_seed`, once per criterion and on one thread, so an interval is the same on every run and does not
//! depend on the number of threads or cores. A gate of the Go bench, whose seed is a pair for Go's PCG, is
//! refused with [`GateError::GoGate`]: its archived reports are kept as records and never re-scored.

use std::collections::BTreeMap;
use std::io;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use serde::{Deserialize, Serialize};
use togi_machine::object_only;
use togi_sim::saturating_count;

use crate::record::{Record, Status};
use crate::stats::percentile;
use crate::suite::Suite;

/// The most resamples a gate may ask for; the bootstrap holds one value per resample.
pub const MAX_RESAMPLES: usize = 10_000_000;

#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("decode gate")]
    Decode(#[from] serde_json::Error),
    #[error(
        "gate: bootstrap_seed is a pair, the seed of the Go bench's PCG; archived Go gates are kept as records and are not re-scored"
    )]
    GoGate,
    #[error("gate needs an id and a baseline ruleset and commits")]
    NoBaseline,
    #[error("gate {gate}: needs gated scenarios and quantiles")]
    NoCriteria { gate: String },
    #[error("gate {gate}: {what} {name:?} is listed twice")]
    Repeated {
        gate: String,
        what: &'static str,
        name: String,
    },
    #[error("gate {gate}: pooled scenario {name:?} is not gated")]
    PooledNotGated { gate: String, name: String },
    #[error(
        "gate {gate}: scenario {name:?} is both gated and concluded; gated scenarios already conclude"
    )]
    ConcludeGated { gate: String, name: String },
    #[error("gate {gate}: quantile {quantile} outside (0, 1)")]
    Quantile { gate: String, quantile: f64 },
    #[error(
        "gate {gate}: needs confidence in (0, 1), resamples from 1 to {MAX_RESAMPLES} and positive max_time_ratio"
    )]
    Statistics { gate: String },
    #[error("gate {gate}: unknown scenario {name:?}")]
    UnknownScenario { gate: String, name: String },
    #[error("gate {gate}: scenario {name} has no {split} seeds")]
    NoSeeds {
        gate: String,
        name: String,
        split: &'static str,
    },
}

/// The seeds a gate is judged on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateSplit {
    Dev,
    Holdout,
    All,
}

impl GateSplit {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Holdout => "holdout",
            Self::All => "all",
        }
    }
}

object_only! {
    /// The recorded runs a gate is judged against: every baseline run must carry the ruleset and one of the
    /// commits, so a pull request that re-records some runs lists its recording commit here.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct GateBaseline {
        pub ruleset: u32,
        pub commits: Vec<String>,
    }

    /// A ruleset's bench gate as the suite file registers it.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Gate {
        pub id: String,
        /// The rationale, printed with the report.
        #[serde(default)]
        pub note: String,
        /// Rationale for reviewers, printed by the run; never scored.
        #[serde(default)]
        pub notes: Vec<String>,
        pub baseline: GateBaseline,
        pub split: GateSplit,
        pub gated: Vec<String>,
        #[serde(default)]
        pub pooled: Vec<String>,
        #[serde(default)]
        pub conclude: Vec<String>,
        pub quantiles: Vec<f64>,
        pub confidence: f64,
        pub resamples: usize,
        pub bootstrap_seed: u64,
        pub max_time_ratio: f64,
    }
}

/// A gate checked against the suite's scenarios, with the seeds it requires of each.
#[derive(Clone, Debug)]
pub struct Resolved {
    gate: Gate,
    seeds: BTreeMap<String, Vec<u64>>,
}

/// Reads the suite's registered gate, or none when it registers none.
pub fn load(suite: &Suite) -> Result<Option<Resolved>, GateError> {
    let Some(object) = suite.gate_object() else {
        return Ok(None);
    };
    if object
        .get("bootstrap_seed")
        .is_some_and(serde_json::Value::is_array)
    {
        return Err(GateError::GoGate);
    }
    let gate: Gate = serde_json::from_value(serde_json::Value::Object(object.clone()))?;
    Resolved::new(gate, |name| suite.seeds_of(name)).map(Some)
}

fn repeated<'a>(
    gate: &str,
    what: &'static str,
    names: impl IntoIterator<Item = &'a String>,
) -> Result<(), GateError> {
    let mut seen = std::collections::BTreeSet::new();
    for name in names {
        if !seen.insert(name) {
            return Err(GateError::Repeated {
                gate: gate.to_owned(),
                what,
                name: name.clone(),
            });
        }
    }
    Ok(())
}

impl Resolved {
    /// Checks `gate`, and records the seeds of its split for every scenario it names; `seeds_of` gives a
    /// scenario's dev and holdout seeds, or none for a scenario the suite lacks.
    pub fn new<'a>(
        gate: Gate,
        seeds_of: impl Fn(&str) -> Option<(&'a [u64], &'a [u64])>,
    ) -> Result<Self, GateError> {
        let id = gate.id.clone();
        if gate.id.is_empty() || gate.baseline.ruleset == 0 || gate.baseline.commits.is_empty() {
            return Err(GateError::NoBaseline);
        }
        if gate.baseline.commits.iter().any(String::is_empty) {
            return Err(GateError::NoBaseline);
        }
        if gate.gated.is_empty() || gate.quantiles.is_empty() {
            return Err(GateError::NoCriteria { gate: id });
        }
        repeated(&id, "gated scenario", &gate.gated)?;
        repeated(&id, "pooled scenario", &gate.pooled)?;
        repeated(&id, "concluded scenario", &gate.conclude)?;
        for name in &gate.pooled {
            if !gate.gated.contains(name) {
                return Err(GateError::PooledNotGated {
                    gate: id,
                    name: name.clone(),
                });
            }
        }
        for name in &gate.conclude {
            if gate.gated.contains(name) {
                return Err(GateError::ConcludeGated {
                    gate: id,
                    name: name.clone(),
                });
            }
        }
        for &quantile in &gate.quantiles {
            if !(quantile > 0.0 && quantile < 1.0) {
                return Err(GateError::Quantile { gate: id, quantile });
            }
        }
        let labels: Vec<String> = gate.quantiles.iter().map(|&q| quantile_label(q)).collect();
        repeated(&id, "quantile", &labels)?;
        if !(gate.confidence > 0.0 && gate.confidence < 1.0)
            || gate.resamples == 0
            || gate.resamples > MAX_RESAMPLES
            || !(gate.max_time_ratio > 0.0 && gate.max_time_ratio.is_finite())
        {
            return Err(GateError::Statistics { gate: id });
        }
        let mut seeds = BTreeMap::new();
        for name in gate.gated.iter().chain(&gate.conclude) {
            let (dev, holdout) = seeds_of(name).ok_or_else(|| GateError::UnknownScenario {
                gate: id.clone(),
                name: name.clone(),
            })?;
            let mut chosen = Vec::new();
            if gate.split != GateSplit::Holdout {
                chosen.extend_from_slice(dev);
            }
            if gate.split != GateSplit::Dev {
                chosen.extend_from_slice(holdout);
            }
            if chosen.is_empty() {
                return Err(GateError::NoSeeds {
                    gate: id,
                    name: name.clone(),
                    split: gate.split.as_str(),
                });
            }
            seeds.insert(name.clone(), chosen);
        }
        Ok(Self { gate, seeds })
    }

    pub const fn gate(&self) -> &Gate {
        &self.gate
    }

    /// The seeds the gate requires of a gated or concluded scenario, in suite order.
    pub fn seeds(&self, scenario: &str) -> Option<&[u64]> {
        self.seeds.get(scenario).map(Vec::as_slice)
    }

    /// Scores `candidate` against `baseline`. It refuses unless the baseline is the one the gate names and
    /// every required seed is paired with the metrics the criteria need.
    pub fn judge(&self, candidate: &[Record], baseline: &[Record]) -> Verdict {
        let pairs = match self.pair(candidate, baseline) {
            Ok(pairs) => pairs,
            Err(reason) => return Verdict::Refused(reason),
        };
        let gate = &self.gate;
        let mut criteria = Vec::new();
        for name in gate.gated.iter().chain(&gate.conclude) {
            criteria.push(conclusion(name, &pairs[name.as_str()]));
        }
        let mut pooled: Vec<(f64, f64)> = Vec::new();
        for name in &gate.gated {
            let hazards = hazards(&pairs[name.as_str()]);
            if gate.pooled.contains(name) {
                pooled.extend_from_slice(&hazards);
            }
            for &q in &gate.quantiles {
                let change = quantile_change(
                    &hazards,
                    q,
                    gate.confidence,
                    gate.resamples,
                    gate.bootstrap_seed,
                );
                criteria.push(Criterion {
                    scenario: name.clone(),
                    pass: change.lo <= 0.0,
                    measure: Measure::Quantile {
                        label: quantile_label(q),
                        baseline: change.baseline,
                        candidate: change.candidate,
                        lo: change.lo,
                        hi: change.hi,
                    },
                });
            }
            criteria.push(time(name, &pairs[name.as_str()], gate.max_time_ratio));
        }
        if !gate.pooled.is_empty() {
            let (mut cand, mut base): (Vec<f64>, Vec<f64>) = pooled.into_iter().unzip();
            cand.sort_by(f64::total_cmp);
            base.sort_by(f64::total_cmp);
            let (baseline, candidate) = (percentile(&base, 0.5), percentile(&cand, 0.5));
            criteria.push(Criterion {
                scenario: gate.pooled.join("+"),
                pass: candidate < baseline,
                measure: Measure::PooledMedian {
                    baseline,
                    candidate,
                },
            });
        }
        Verdict::Scored(criteria)
    }

    fn pair<'a>(
        &self,
        candidate: &'a [Record],
        baseline: &'a [Record],
    ) -> Result<BTreeMap<&str, Vec<Pair<'a>>>, String> {
        let gate = &self.gate;
        for run in baseline {
            if run.ruleset != gate.baseline.ruleset || !gate.baseline.commits.contains(&run.commit)
            {
                return Err(format!(
                    "baseline run {}/{} is ruleset {} at {}, not ruleset {} at {}",
                    run.scenario,
                    run.seed,
                    run.ruleset,
                    run.commit,
                    gate.baseline.ruleset,
                    gate.baseline.commits.join(" or "),
                ));
            }
        }
        let index = |records: &'a [Record]| -> BTreeMap<(&'a str, u64), &'a Record> {
            records.iter().map(|run| (run.key(), run)).collect()
        };
        let (cand, base) = (index(candidate), index(baseline));
        let mut pairs = BTreeMap::new();
        for name in gate.gated.iter().chain(&gate.conclude) {
            let mut scenario = Vec::new();
            for &seed in &self.seeds[name] {
                match (
                    cand.get(&(name.as_str(), seed)),
                    base.get(&(name.as_str(), seed)),
                ) {
                    (Some(&candidate), Some(&baseline)) => scenario.push(Pair {
                        candidate,
                        baseline,
                    }),
                    (a, b) => {
                        let missing = match (a, b) {
                            (None, None) => "candidate or baseline",
                            (None, Some(_)) => "candidate",
                            _ => "baseline",
                        };
                        return Err(format!(
                            "incomplete pairing: {name} seed {seed} has no {missing} run; the gate needs every {} seed of its scenarios",
                            gate.split.as_str()
                        ));
                    }
                }
            }
            pairs.insert(name.as_str(), scenario);
        }
        for name in &gate.gated {
            for pair in &pairs[name.as_str()] {
                if pair.candidate.worst_r7_hazard_per_h.is_none()
                    || pair.baseline.worst_r7_hazard_per_h.is_none()
                {
                    return Err(format!(
                        "{name} seed {} lacks worst_r7_hazard_per_h on a run",
                        pair.candidate.seed
                    ));
                }
            }
        }
        Ok(pairs)
    }
}

#[derive(Clone, Copy)]
struct Pair<'a> {
    candidate: &'a Record,
    baseline: &'a Record,
}

fn conclusion(name: &str, pairs: &[Pair<'_>]) -> Criterion {
    let (mut baseline, mut candidate) = (0, 0);
    let mut lost = Vec::new();
    for pair in pairs {
        if pair.baseline.status == Status::Concluded {
            baseline += 1;
            if pair.candidate.status != Status::Concluded {
                lost.push(pair.candidate.seed);
            }
        }
        if pair.candidate.status == Status::Concluded {
            candidate += 1;
        }
    }
    Criterion {
        scenario: name.to_owned(),
        pass: lost.is_empty(),
        measure: Measure::Conclusion {
            baseline,
            candidate,
            lost,
        },
    }
}

/// Each pair's worst R7 hazard as (candidate, baseline); every pair has both, as `pair` checked.
fn hazards(pairs: &[Pair<'_>]) -> Vec<(f64, f64)> {
    pairs
        .iter()
        .filter_map(|pair| {
            Some((
                pair.candidate.worst_r7_hazard_per_h?,
                pair.baseline.worst_r7_hazard_per_h?,
            ))
        })
        .collect()
}

fn time(name: &str, pairs: &[Pair<'_>], max_ratio: f64) -> Criterion {
    let mut logs = 0.0;
    let mut timed = 0_usize;
    for pair in pairs {
        let (c, b) = (pair.candidate, pair.baseline);
        if c.status == Status::Concluded
            && b.status == Status::Concluded
            && c.sim_hours > 0.0
            && b.sim_hours > 0.0
        {
            logs += (c.sim_hours / b.sim_hours).ln();
            timed += 1;
        }
    }
    let ratio = (timed > 0).then(|| (logs / saturating_count(timed)).exp());
    Criterion {
        scenario: name.to_owned(),
        pass: ratio.is_some_and(|ratio| ratio <= max_ratio),
        measure: Measure::Time { ratio, timed },
    }
}

/// `median` for 0.5, else `p` and the percentage without trailing zeros: `p90`, `p99.5`.
pub fn quantile_label(q: f64) -> String {
    if q.to_bits() == 0.5_f64.to_bits() {
        "median".to_owned()
    } else {
        format!("p{}", percent(q))
    }
}

/// `fraction` as a percentage with at most six decimals and no trailing zeros.
fn percent(fraction: f64) -> String {
    let text = format!("{:.6}", fraction * 100.0);
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A quantile of each side and the interval of the candidate's minus the baseline's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Change {
    pub baseline: f64,
    pub candidate: f64,
    pub lo: f64,
    pub hi: f64,
}

/// The `q`-quantile of each side of `pairs`, as (candidate, baseline), and the bootstrap interval of the
/// candidate's minus the baseline's, resampling pairs: each resample draws `pairs.len()` pair indices with
/// replacement from one generator seeded with `seed`, so the interval is the same on every run.
pub fn quantile_change(
    pairs: &[(f64, f64)],
    q: f64,
    confidence: f64,
    resamples: usize,
    seed: u64,
) -> Change {
    let n = pairs.len();
    let side = |pick: fn(&(f64, f64)) -> f64| {
        let mut values: Vec<f64> = pairs.iter().map(pick).collect();
        values.sort_by(f64::total_cmp);
        percentile(&values, q)
    };
    let (candidate, baseline) = (side(|p| p.0), side(|p| p.1));
    if n == 0 {
        return Change {
            baseline,
            candidate,
            lo: 0.0,
            hi: 0.0,
        };
    }
    let mut rng = StdRng::seed_from_u64(seed);
    let (mut drawn_candidate, mut drawn_baseline) = (vec![0.0; n], vec![0.0; n]);
    let mut boot = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        for (x, y) in drawn_candidate.iter_mut().zip(&mut drawn_baseline) {
            (*x, *y) = pairs[rng.random_range(0..n)];
        }
        drawn_candidate.sort_by(f64::total_cmp);
        drawn_baseline.sort_by(f64::total_cmp);
        boot.push(percentile(&drawn_candidate, q) - percentile(&drawn_baseline, q));
    }
    boot.sort_by(f64::total_cmp);
    let tail = (1.0 - confidence) / 2.0;
    Change {
        baseline,
        candidate,
        lo: percentile(&boot, tail),
        hi: percentile(&boot, 1.0 - tail),
    }
}

/// One judged criterion of a gate.
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    pub scenario: String,
    pub pass: bool,
    pub measure: Measure,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Measure {
    /// Every seed the baseline concluded is still concluded.
    Conclusion {
        baseline: usize,
        candidate: usize,
        lost: Vec<u64>,
    },
    /// A quantile of the worst R7 hazard; fails only when the interval of the change lies above 0.
    Quantile {
        label: String,
        baseline: f64,
        candidate: f64,
        lo: f64,
        hi: f64,
    },
    /// The geometric mean of simulated-time ratios over the pairs both concluded; none when no pair did.
    Time { ratio: Option<f64>, timed: usize },
    /// The pooled median must fall strictly.
    PooledMedian { baseline: f64, candidate: f64 },
}

/// A gate's verdict; `Refused` names why it could not be judged.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Refused(String),
    Scored(Vec<Criterion>),
}

impl Verdict {
    pub fn pass(&self) -> bool {
        match self {
            Self::Refused(_) => false,
            Self::Scored(criteria) => criteria.iter().all(|c| c.pass),
        }
    }
}

/// A gate and the verdict it gave a run.
#[derive(Clone, Debug)]
pub struct Judgment {
    pub resolved: Resolved,
    pub verdict: Verdict,
}

impl Judgment {
    pub fn passed(&self) -> bool {
        self.verdict.pass()
    }

    pub fn write(&self, w: &mut impl io::Write) -> io::Result<()> {
        report(w, &self.resolved.gate, &self.verdict)
    }
}

/// Prints the gate's header, each criterion with its estimate, interval, threshold and `PASS` or `FAIL`, and
/// the overall result.
pub fn report(w: &mut impl io::Write, gate: &Gate, verdict: &Verdict) -> io::Result<()> {
    writeln!(
        w,
        "gate {}: judged against ruleset {} at {} on every {} seed; quantiles of worst_r7_hazard_per_h fail only when the {}% interval of candidate minus baseline ({} paired resamples, seed {}) lies above 0; time is the geometric mean ratio over pairs both concluded.",
        gate.id,
        gate.baseline.ruleset,
        gate.baseline.commits.join(" or "),
        gate.split.as_str(),
        percent(gate.confidence),
        gate.resamples,
        gate.bootstrap_seed,
    )?;
    if !gate.note.is_empty() {
        writeln!(w, "gate {}: {}", gate.id, gate.note)?;
    }
    let criteria = match verdict {
        Verdict::Refused(reason) => return writeln!(w, "gate {}: FAIL refused: {reason}", gate.id),
        Verdict::Scored(criteria) => criteria,
    };
    for c in criteria {
        let result = if c.pass { "PASS" } else { "FAIL" };
        let scenario = &c.scenario;
        match &c.measure {
            Measure::Conclusion {
                baseline,
                candidate,
                lost,
            } => {
                let seeds: Vec<String> = lost.iter().map(u64::to_string).collect();
                writeln!(
                    w,
                    "gate conclusion {scenario} baseline_concluded={baseline} candidate_concluded={candidate} lost={} lost_seeds=[{}] threshold=lost==0 {result}",
                    lost.len(),
                    seeds.join(","),
                )?;
            }
            Measure::Time { ratio, timed } => {
                let ratio = ratio.map_or_else(|| "none".to_owned(), |r| format!("{r:.4}"));
                writeln!(
                    w,
                    "gate time {scenario} ratio={ratio} timed={timed} threshold=ratio<={} {result}",
                    gate.max_time_ratio,
                )?;
            }
            Measure::PooledMedian {
                baseline,
                candidate,
            } => writeln!(
                w,
                "gate pooled_median {scenario} baseline={baseline:.6} candidate={candidate:.6} change={:+.6} threshold=change<0 {result}",
                candidate - baseline,
            )?,
            Measure::Quantile {
                label,
                baseline,
                candidate,
                lo,
                hi,
            } => writeln!(
                w,
                "gate {label} {scenario} baseline={baseline:.6} candidate={candidate:.6} change={:+.6} ci=[{lo:+.6},{hi:+.6}] threshold=ci_low<=0 {result}",
                candidate - baseline,
            )?,
        }
    }
    let failed = criteria.iter().filter(|c| !c.pass).count();
    if failed > 0 {
        writeln!(
            w,
            "gate {}: FAIL {failed} of {} criteria",
            gate.id,
            criteria.len()
        )
    } else {
        writeln!(w, "gate {}: PASS {} criteria", gate.id, criteria.len())
    }
}
