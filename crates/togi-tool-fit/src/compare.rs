//! Pairs a candidate's forward records with a reference's, trial by trial, and compares them exactly: the
//! loss, surprises and calibration of every held-out session and of the pool, for all trials and the
//! single-core, non-R7 and R7 subsets, the program's primary and F1–F3 checks, and a paired trial bootstrap
//! of each session's and the pool's loss ratio over all its trials.
//!
//! Both runs must be [`crate::records`] of the forward-chained check on the same rows with the same seal:
//! the reference is the unchanged fitter's prefix fits, never a fixed ensemble. Sums run per session in
//! trial order and pool the sessions in order, as the check itself does, so a pooled sum here equals the
//! check's own.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use togi_journal::SessionId;
use togi_machine::{Regime, WorkloadId};

use crate::error::Error;
use crate::records::Record;

/// Resamples of each paired bootstrap.
pub const RESAMPLES: usize = 10_000;
/// The seed of each paired bootstrap's [`StdRng`].
pub const BOOTSTRAP_SEED: u64 = 0;

/// The trials a row of the comparison covers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Subset {
    All,
    SingleCore,
    NonR7,
    R7,
}

pub const SUBSETS: [Subset; 4] = [Subset::All, Subset::SingleCore, Subset::NonR7, Subset::R7];

impl Subset {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::SingleCore => "single-core",
            Self::NonR7 => "non-R7",
            Self::R7 => "R7",
        }
    }

    fn contains(self, workload: WorkloadId, loaded: usize) -> bool {
        match self {
            Self::All => true,
            Self::SingleCore => loaded == 1,
            Self::NonR7 => togi_sim::modeled_as(workload.regime()) != Regime::R7,
            Self::R7 => togi_sim::modeled_as(workload.regime()) == Regime::R7,
        }
    }
}

/// One side's sums over a set of trials.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Side {
    pub fit_log_loss: f64,
    pub predicted: f64,
    pub surprises: usize,
}

impl Side {
    fn add(&mut self, other: &Self) {
        self.fit_log_loss += other.fit_log_loss;
        self.predicted += other.predicted;
        self.surprises += other.surprises;
    }

    /// `|ln(predicted / failures)|`; `None` without failures, where the predicted count stands instead.
    pub fn calibration(&self, failures: usize) -> Option<f64> {
        (failures > 0).then(|| (self.predicted / count(failures)).ln().abs())
    }
}

/// Paired sums over a set of trials.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tally {
    pub trials: usize,
    pub failures: usize,
    pub constant_log_loss: f64,
    pub candidate: Side,
    pub reference: Side,
}

impl Tally {
    fn add(&mut self, other: &Self) {
        self.trials += other.trials;
        self.failures += other.failures;
        self.constant_log_loss += other.constant_log_loss;
        self.candidate.add(&other.candidate);
        self.reference.add(&other.reference);
    }

    /// The candidate's summed loss over the reference's.
    pub fn loss_ratio(&self) -> f64 {
        self.candidate.fit_log_loss / self.reference.fit_log_loss
    }
}

/// One held-out session's tallies, or the pool's, by [`SUBSETS`] order, and the paired bootstrap interval of
/// its loss ratio over all its trials.
#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    pub tallies: [Tally; 4],
    pub interval: (f64, f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comparison {
    pub sessions: Vec<(SessionId, Scope)>,
    pub pooled: Scope,
}

impl Comparison {
    /// Held-out sessions where the candidate's loss is strictly lower than the reference's.
    pub fn improved(&self) -> usize {
        self.sessions
            .iter()
            .filter(|(_, scope)| {
                let all = &scope.tallies[0];
                all.candidate.fit_log_loss < all.reference.fit_log_loss
            })
            .count()
    }
}

/// One trial's paired values: the per-trial losses of both sides and what they share.
#[derive(Clone, Copy, Debug)]
struct Pair {
    session: SessionId,
    workload: WorkloadId,
    loaded: usize,
    failure: bool,
    constant_log_loss: f64,
    candidate: Side,
    reference: Side,
}

/// Pairs `candidate` with `reference` and compares them, the interval of each session's and the pool's
/// all-trials loss ratio from `resamples` resamples of its trials seeded `seed`. The two must hold the same
/// input, seal and trials in the same order with the same outcomes and constants; anything else is unpaired.
pub fn compare(
    candidate: &[Record],
    reference: &[Record],
    resamples: usize,
    seed: u64,
) -> Result<Comparison, Error> {
    let pairs = pair(candidate, reference)?;
    let mut sessions: Vec<(SessionId, Scope)> = Vec::new();
    let mut pooled = [Tally::default(); 4];
    let mut begin = 0;
    while begin < pairs.len() {
        let session = pairs[begin].session;
        let end = begin
            + pairs[begin..]
                .iter()
                .take_while(|pair| pair.session == session)
                .count();
        let held_out = &pairs[begin..end];
        let tallies = tally(held_out);
        for (pool, tally) in pooled.iter_mut().zip(&tallies) {
            pool.add(tally);
        }
        sessions.push((
            session,
            Scope {
                tallies,
                interval: bootstrap(held_out, resamples, seed),
            },
        ));
        begin = end;
    }
    Ok(Comparison {
        sessions,
        pooled: Scope {
            tallies: pooled,
            interval: bootstrap(&pairs, resamples, seed),
        },
    })
}

fn pair(candidate: &[Record], reference: &[Record]) -> Result<Vec<Pair>, Error> {
    let unpaired = |why: String| Error::Unpaired(why);
    let input = |records: &[Record]| {
        records.iter().find_map(|record| match record {
            Record::Input { sha256, rows, seal } => Some((sha256.clone(), *rows, *seal)),
            Record::Trial { .. } | Record::Session { .. } | Record::Pooled { .. } => None,
        })
    };
    match (input(candidate), input(reference)) {
        (Some(c), Some(r)) if c == r => {}
        (Some(c), Some(r)) => {
            return Err(unpaired(format!(
                "the candidate read sha256 {} ({} rows) with --seal {}, the reference sha256 {} ({} rows) with --seal {}",
                c.0, c.1, c.2, r.0, r.1, r.2
            )));
        }
        (None, _) | (_, None) => return Err(unpaired("a records file has no input line".into())),
    }
    let trials = |records: &[Record]| {
        records
            .iter()
            .filter(|record| matches!(record, Record::Trial { .. }))
            .count()
    };
    let (scored, reference_scored) = (trials(candidate), trials(reference));
    if scored != reference_scored {
        return Err(unpaired(format!(
            "the candidate scored {scored} trials, the reference {reference_scored}"
        )));
    }
    let only_trials = |record: &&Record| matches!(record, Record::Trial { .. });
    let mut pairs = Vec::with_capacity(scored);
    for (c, r) in candidate
        .iter()
        .filter(only_trials)
        .zip(reference.iter().filter(only_trials))
    {
        let (
            Record::Trial {
                session,
                seq,
                training_sessions,
                workload,
                loaded,
                failure,
                probability,
                constant,
                fit_log_loss,
                constant_log_loss,
                surprise,
                matched: _,
            },
            Record::Trial {
                session: r_session,
                seq: r_seq,
                training_sessions: r_training,
                workload: r_workload,
                loaded: r_loaded,
                failure: r_failure,
                probability: r_probability,
                constant: r_constant,
                fit_log_loss: r_loss,
                constant_log_loss: r_constant_loss,
                surprise: r_surprise,
                matched: _,
            },
        ) = (c, r)
        else {
            continue;
        };
        let same = session == r_session
            && seq == r_seq
            && training_sessions == r_training
            && workload == r_workload
            && loaded == r_loaded
            && failure == r_failure
            && constant.to_bits() == r_constant.to_bits()
            && constant_log_loss.to_bits() == r_constant_loss.to_bits();
        if !same {
            return Err(unpaired(format!(
                "trial {session}:{seq} of the candidate meets {r_session}:{r_seq} of the reference with a different outcome, class or constant"
            )));
        }
        pairs.push(Pair {
            session: *session,
            workload: *workload,
            loaded: *loaded,
            failure: *failure,
            constant_log_loss: *constant_log_loss,
            candidate: Side {
                fit_log_loss: *fit_log_loss,
                predicted: *probability,
                surprises: usize::from(*surprise),
            },
            reference: Side {
                fit_log_loss: *r_loss,
                predicted: *r_probability,
                surprises: usize::from(*r_surprise),
            },
        });
    }
    Ok(pairs)
}

fn tally(pairs: &[Pair]) -> [Tally; 4] {
    let mut tallies = [Tally::default(); 4];
    for pair in pairs {
        let one = Tally {
            trials: 1,
            failures: usize::from(pair.failure),
            constant_log_loss: pair.constant_log_loss,
            candidate: pair.candidate,
            reference: pair.reference,
        };
        for (subset, tally) in SUBSETS.iter().zip(&mut tallies) {
            if subset.contains(pair.workload, pair.loaded) {
                tally.add(&one);
            }
        }
    }
    tallies
}

/// The 2.5% and 97.5% quantiles (linear interpolation) of the candidate-to-reference loss ratio over
/// `resamples` draws of `pairs.len()` trials with replacement, both sides of a drawn trial together.
fn bootstrap(pairs: &[Pair], resamples: usize, seed: u64) -> (f64, f64) {
    if pairs.is_empty() || resamples == 0 {
        return (f64::NAN, f64::NAN);
    }
    let mut rng = StdRng::seed_from_u64(seed);
    let mut ratios: Vec<f64> = (0..resamples)
        .map(|_| {
            let (mut candidate, mut reference) = (0.0, 0.0);
            for _ in 0..pairs.len() {
                let pair = &pairs[rng.random_range(0..pairs.len())];
                candidate += pair.candidate.fit_log_loss;
                reference += pair.reference.fit_log_loss;
            }
            candidate / reference
        })
        .collect();
    ratios.sort_by(f64::total_cmp);
    interval(&ratios)
}

/// The 2.5% and 97.5% quantiles of the ascending `sorted`, linearly interpolated between neighbours.
pub(crate) fn interval(sorted: &[f64]) -> (f64, f64) {
    (quantile(sorted, 0.025), quantile(sorted, 0.975))
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    let position = q * count(sorted.len() - 1);
    let below = position.floor();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a nonnegative index below the slice's length"
    )]
    let index = below as usize;
    let above = sorted.get(index + 1).copied().unwrap_or(sorted[index]);
    sorted[index] + (position - below) * (above - sorted[index])
}

fn count(n: usize) -> f64 {
    togi_sim::saturating_count(n)
}

/// Prints `comparison` at full precision, then the program's primary and F1–F3 checks on the pool.
pub fn report(out: &mut impl Write, comparison: &Comparison, resamples: usize) -> io::Result<()> {
    writeln!(
        out,
        "Paired forward comparison: candidate against reference; losses are per trial; each interval is the 2.5% and 97.5% quantiles of the loss ratio over {resamples} paired resamples of its trials"
    )?;
    for (session, scope) in &comparison.sessions {
        report_scope(out, &session.to_string(), scope)?;
    }
    report_scope(out, "Pooled", &comparison.pooled)?;
    let all = &comparison.pooled.tallies[0];
    let holds = |ok: bool| if ok { "holds" } else { "fails" };
    writeln!(
        out,
        "Primary: pooled loss candidate={} reference={}: {}",
        per_trial(all.candidate.fit_log_loss, all.trials),
        per_trial(all.reference.fit_log_loss, all.trials),
        if all.candidate.fit_log_loss < all.reference.fit_log_loss {
            "improved"
        } else {
            "not improved"
        }
    )?;
    writeln!(
        out,
        "F1: pooled failures_p<0.01 candidate={} reference={}: {}",
        all.candidate.surprises,
        all.reference.surprises,
        holds(all.candidate.surprises <= all.reference.surprises)
    )?;
    match (
        all.candidate.calibration(all.failures),
        all.reference.calibration(all.failures),
    ) {
        (Some(candidate), Some(reference)) => writeln!(
            out,
            "F2: pooled |ln(predicted/failures)| candidate={candidate} reference={reference}: {}",
            holds(candidate <= reference)
        )?,
        (None, _) | (_, None) => writeln!(
            out,
            "F2: no pooled failures; predicted candidate={} reference={}",
            all.candidate.predicted, all.reference.predicted
        )?,
    }
    let sessions = comparison.sessions.len();
    let improved = comparison.improved();
    writeln!(
        out,
        "F3: candidate loss lower in {improved} of {sessions} held-out sessions: {}",
        holds(2 * improved > sessions)
    )
}

fn report_scope(out: &mut impl Write, name: &str, scope: &Scope) -> io::Result<()> {
    for (subset, tally) in SUBSETS.iter().zip(&scope.tallies) {
        if tally.trials == 0 {
            continue;
        }
        write!(
            out,
            "{name} {}: trials={} failures={} loss candidate={} reference={} constant={} ratio={} failures_p<0.01 candidate={} reference={} predicted candidate={} reference={}",
            subset.label(),
            tally.trials,
            tally.failures,
            per_trial(tally.candidate.fit_log_loss, tally.trials),
            per_trial(tally.reference.fit_log_loss, tally.trials),
            per_trial(tally.constant_log_loss, tally.trials),
            tally.loss_ratio(),
            tally.candidate.surprises,
            tally.reference.surprises,
            tally.candidate.predicted,
            tally.reference.predicted,
        )?;
        match (
            tally.candidate.calibration(tally.failures),
            tally.reference.calibration(tally.failures),
        ) {
            (Some(candidate), Some(reference)) => write!(
                out,
                " |ln(predicted/failures)| candidate={candidate} reference={reference}"
            )?,
            (None, _) | (_, None) => write!(out, " |ln(predicted/failures)| none: no failures")?,
        }
        if *subset == Subset::All {
            write!(
                out,
                " ratio_95%=[{}, {}]",
                scope.interval.0, scope.interval.1
            )?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn per_trial(sum: f64, trials: usize) -> f64 {
    sum / count(trials)
}

/// The comparison as a flat object of numbers, the form a campaign's outcome records: `SCOPE.SUBSET.NAME`
/// for each held-out session and `pooled`, then `pooled.f3_improved` and `pooled.f3_sessions`. A subset
/// without failures has no calibration keys, and a value JSON cannot hold, such as the infinite calibration
/// of a side that predicted no failure, is left out; the report prints it.
pub fn metrics(comparison: &Comparison) -> BTreeMap<String, f64> {
    let mut metrics = BTreeMap::new();
    let mut scope = |name: &str, scope: &Scope| {
        for (subset, tally) in SUBSETS.iter().zip(&scope.tallies) {
            if tally.trials == 0 {
                continue;
            }
            let mut put = |key: &str, value: f64| {
                if value.is_finite() {
                    metrics.insert(format!("{name}.{}.{key}", subset.label()), value);
                }
            };
            put("trials", count(tally.trials));
            put("failures", count(tally.failures));
            put(
                "candidate_loss",
                per_trial(tally.candidate.fit_log_loss, tally.trials),
            );
            put(
                "reference_loss",
                per_trial(tally.reference.fit_log_loss, tally.trials),
            );
            put(
                "constant_loss",
                per_trial(tally.constant_log_loss, tally.trials),
            );
            put("loss_ratio", tally.loss_ratio());
            put("candidate_surprises", count(tally.candidate.surprises));
            put("reference_surprises", count(tally.reference.surprises));
            put("candidate_predicted", tally.candidate.predicted);
            put("reference_predicted", tally.reference.predicted);
            if let (Some(candidate), Some(reference)) = (
                tally.candidate.calibration(tally.failures),
                tally.reference.calibration(tally.failures),
            ) {
                put("candidate_calibration", candidate);
                put("reference_calibration", reference);
            }
            if *subset == Subset::All {
                put("ratio_low", scope.interval.0);
                put("ratio_high", scope.interval.1);
            }
        }
    };
    for (session, tallies) in &comparison.sessions {
        scope(&session.to_string(), tallies);
    }
    scope("pooled", &comparison.pooled);
    metrics.insert("pooled.f3_improved".into(), count(comparison.improved()));
    metrics.insert(
        "pooled.f3_sessions".into(),
        count(comparison.sessions.len()),
    );
    metrics
}

/// Compares the records files `candidate` and `reference`, printing the report to `out` and, with
/// `metrics_out`, writing [`metrics`] as JSON to that new file.
pub fn compare_files(
    candidate: &Path,
    reference: &Path,
    metrics_out: Option<&Path>,
    out: &mut impl Write,
) -> Result<(), Error> {
    let comparison = compare(
        &crate::records::read(candidate)?,
        &crate::records::read(reference)?,
        RESAMPLES,
        BOOTSTRAP_SEED,
    )?;
    report(out, &comparison, RESAMPLES)?;
    if let Some(path) = metrics_out {
        let io = |action: &'static str| {
            move |source| Error::Io {
                action,
                path: path.to_owned(),
                source,
            }
        };
        let mut file = BufWriter::new(File::create_new(path).map_err(io("create the metrics"))?);
        serde_json::to_writer_pretty(&mut file, &metrics(&comparison))
            .map_err(io::Error::from)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.flush())
            .map_err(io("write the metrics"))?;
    }
    Ok(())
}
