use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use togi_sim::saturating_count;

use crate::record::{Record, Status};
use crate::stats::{percentile, share};

const RESAMPLES: usize = 10_000;
const BOOTSTRAP_SEED: u64 = 1;
const HAZARD_RISE_PER_H: f64 = 0.01;
const TARGET: &str = "target";

/// A run and the baseline's run of the same scenario and seed.
#[derive(Clone, Copy, Debug)]
pub struct Pair<'a> {
    pub candidate: &'a Record,
    pub baseline: &'a Record,
}

/// The candidate's runs that the baseline also has, in scenario then seed order.
pub fn pairing<'a>(candidate: &'a [Record], baseline: &'a [Record]) -> Vec<Pair<'a>> {
    let base: BTreeMap<_, _> = baseline.iter().map(|r| (r.key(), r)).collect();
    let mut pairs: Vec<_> = candidate
        .iter()
        .filter_map(|r| {
            base.get(&r.key()).map(|&baseline| Pair {
                candidate: r,
                baseline,
            })
        })
        .collect();
    pairs.sort_by(|a, b| a.candidate.key().cmp(&b.candidate.key()));
    pairs
}

/// The measures of a set of pairs.
#[derive(Clone, Debug, PartialEq)]
pub struct Comparison {
    pub pairs: usize,
    /// Pairs both sides concluded, which the time ratio covers.
    pub timed: usize,
    /// Candidate over baseline `sim_hours`, the geometric mean of the scenarios' geometric means.
    pub ratio: f64,
    pub lo: f64,
    pub hi: f64,
    pub faster: usize,
    pub slower: usize,
    pub equal: usize,
    pub crash_delta: f64,
    pub max_hazard_delta: f64,
    /// V1: runs that lost their conclusion.
    pub v1: usize,
    /// V2: runs whose maximum hazard rose by more than 0.01 per hour.
    pub v2: usize,
    /// V3: a `target` time ratio above 1.
    pub v3: usize,
    pub real_answer_share: f64,
    pub baseline_real_answer_share: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    Accept,
    Neutral,
    Reject,
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Accept => "ACCEPT",
            Self::Neutral => "NEUTRAL",
            Self::Reject => "REJECT",
        })
    }
}

impl Comparison {
    pub fn verdict(&self) -> Verdict {
        if self.v1 + self.v2 + self.v3 > 0 || self.lo > 1.0 {
            Verdict::Reject
        } else if self.timed > 0 && self.hi < 1.0 {
            Verdict::Accept
        } else {
            Verdict::Neutral
        }
    }
}

/// Compares the pairs: violations V1 to V4, deltas, and the time ratio with a bootstrap interval that
/// resamples the pairs within each scenario and weighs scenarios equally. The resampling draws from a generator
/// with a fixed seed, so the interval is the same however often and wherever it runs.
pub fn compare(pairs: &[Pair<'_>]) -> Comparison {
    let mut c = Comparison {
        pairs: pairs.len(),
        timed: 0,
        ratio: 1.0,
        lo: 1.0,
        hi: 1.0,
        faster: 0,
        slower: 0,
        equal: 0,
        crash_delta: 0.0,
        max_hazard_delta: 0.0,
        v1: 0,
        v2: 0,
        v3: 0,
        real_answer_share: 0.0,
        baseline_real_answer_share: 0.0,
    };
    let mut logs: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let (mut real, mut trials, mut base_real, mut base_trials) = (0, 0, 0, 0);
    for (i, p) in pairs.iter().enumerate() {
        let (a, b) = (p.candidate, p.baseline);
        real += a.real_answers;
        trials += a.trials;
        base_real += b.real_answers;
        base_trials += b.trials;
        if b.status == Status::Concluded && a.status != Status::Concluded {
            c.v1 += 1;
        }
        let hazard = a.hazard_max_per_h - b.hazard_max_per_h;
        if hazard > HAZARD_RISE_PER_H {
            c.v2 += 1;
        }
        if i == 0 || hazard > c.max_hazard_delta {
            c.max_hazard_delta = hazard;
        }
        c.crash_delta += f64::from(a.crashes) - f64::from(b.crashes);
        if a.status == Status::Concluded
            && b.status == Status::Concluded
            && a.sim_hours > 0.0
            && b.sim_hours > 0.0
        {
            logs.entry(&a.scenario)
                .or_default()
                .push((a.sim_hours / b.sim_hours).ln());
            match a.sim_hours.total_cmp(&b.sim_hours) {
                std::cmp::Ordering::Less => c.faster += 1,
                std::cmp::Ordering::Greater => c.slower += 1,
                std::cmp::Ordering::Equal => c.equal += 1,
            }
        }
    }
    c.real_answer_share = share(real, trials);
    c.baseline_real_answer_share = share(base_real, base_trials);
    if c.pairs > 0 {
        c.crash_delta /= saturating_count(c.pairs);
    }
    let scenarios: Vec<&Vec<f64>> = logs.values().collect();
    let mean = |values: &[f64]| values.iter().sum::<f64>() / saturating_count(values.len());
    let mut sum = 0.0;
    for (&name, values) in &logs {
        let mean = mean(values);
        sum += mean;
        c.timed += values.len();
        if name == TARGET && mean.exp() > 1.0 {
            c.v3 += 1;
        }
    }
    if c.timed == 0 {
        return c;
    }
    c.ratio = (sum / saturating_count(scenarios.len())).exp();
    let mut rng = StdRng::seed_from_u64(BOOTSTRAP_SEED);
    let mut boot: Vec<f64> = (0..RESAMPLES)
        .map(|_| {
            let total: f64 = scenarios
                .iter()
                .map(|values| {
                    let drawn: f64 = (0..values.len())
                        .map(|_| values[rng.random_range(0..values.len())])
                        .sum();
                    drawn / saturating_count(values.len())
                })
                .sum();
            (total / saturating_count(scenarios.len())).exp()
        })
        .collect();
    boot.sort_by(f64::total_cmp);
    c.lo = percentile(&boot, 0.025);
    c.hi = percentile(&boot, 0.975);
    c
}

fn comparison_line(w: &mut impl io::Write, label: &str, c: &Comparison) -> io::Result<()> {
    writeln!(
        w,
        "{label} ratio={:.3} ci=[{:.3},{:.3}] pairs={} timed={} faster={} slower={} equal={} crash_delta={:.3} max_hazard_delta={:.6} real_answer_share={:.6} baseline_real_answer_share={:.6} violations=V1:{},V2:{},V3:{}",
        c.ratio,
        c.lo,
        c.hi,
        c.pairs,
        c.timed,
        c.faster,
        c.slower,
        c.equal,
        c.crash_delta,
        c.max_hazard_delta,
        c.real_answer_share,
        c.baseline_real_answer_share,
        c.v1,
        c.v2,
        c.v3,
    )?;
    Ok(())
}

/// Writes the comparison of `candidate` with `baseline`: a line per scenario and the verdict line. A candidate or
/// baseline run without a match, among the baseline runs of the candidate's splits, is warned about.
pub fn report(w: &mut impl io::Write, candidate: &[Record], baseline: &[Record]) -> io::Result<()> {
    writeln!(
        w,
        "comparison: overall ratio weights scenarios equally; CI resamples pairs within each scenario ({RESAMPLES}, fixed seed). V1=lost conclusion; V2=hazard increase >0.01/h; V3=target ratio >1."
    )?;
    let pairs = pairing(candidate, baseline);
    let splits: BTreeSet<_> = candidate.iter().map(|r| r.split).collect();
    let candidate_keys: BTreeSet<_> = candidate.iter().map(Record::key).collect();
    let baseline_keys: BTreeSet<_> = baseline.iter().map(Record::key).collect();
    let unmatched_candidates = candidate
        .iter()
        .filter(|r| !baseline_keys.contains(&r.key()))
        .count();
    let unmatched_baselines = baseline
        .iter()
        .filter(|r| splits.contains(&r.split) && !candidate_keys.contains(&r.key()))
        .count();
    if unmatched_candidates > 0 || unmatched_baselines > 0 {
        writeln!(
            w,
            "comparison: WARNING {unmatched_candidates} candidate and {unmatched_baselines} baseline runs in the same splits unmatched; {} paired",
            pairs.len(),
        )?;
    }
    let mut groups: BTreeMap<&str, Vec<Pair<'_>>> = BTreeMap::new();
    for pair in &pairs {
        groups
            .entry(&pair.candidate.scenario)
            .or_default()
            .push(*pair);
    }
    for (name, group) in &groups {
        comparison_line(w, name, &compare(group))?;
    }
    let all = compare(&pairs);
    comparison_line(w, &format!("verdict: {}", all.verdict()), &all)
}
