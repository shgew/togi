//! The forward-chained check: for each real session after the first, fit the earlier sessions' decisive
//! trials and score the held-out session, so no outcome informs its own prediction.
//!
//! [`reference_check`] scores the same held-out sessions with a fixed ensemble instead of the prefix fits,
//! fitting nothing.

use std::collections::{BTreeMap, HashSet};
use std::io;

use rayon::prelude::*;
use togi_journal::{Seq, SessionId};
use togi_machine::{REGIMES, Regime, WorkloadId};
use togi_sim::{MachineDefinition, Model, saturating_count};
use togi_tool_modelcheck::Checker;
use togi_tool_trialfacts::{ProfileClassKey, TrialFact};

use crate::error::Error;
use crate::evidence::{is_failure, load_of, regime_of, trial_class};
use crate::search::fit;

/// Log loss never uses a probability closer to 0 or 1 than this.
const CLAMP: f64 = 1e-4;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ForwardCounts {
    pub trials: usize,
    pub failures: usize,
    pub predicted: f64,
    pub fit_log_loss: f64,
    pub constant_log_loss: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ForwardScore {
    pub counts: ForwardCounts,
    /// Held-out failures the model gave less than 1% probability.
    pub surprises: usize,
    /// Held-out trials whose class and full profile occur in the earlier sessions.
    pub matches: usize,
    /// Model-check groups flagged on the held-out session; for a reference ensemble, the groups flagged by
    /// at least one member.
    pub flagged: usize,
    pub eligible: usize,
}

/// Which CCDs an R7 load runs on; cores are split in halves, core `c` on CCD `c / (cores / 2)`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum R7Ccd {
    Ccd0,
    Ccd1,
    Both,
}

impl R7Ccd {
    fn label(self) -> &'static str {
        match self {
            Self::Ccd0 => "R7 CCD0 loaded",
            Self::Ccd1 => "R7 CCD1 loaded",
            Self::Both => "R7 both CCDs loaded",
        }
    }
}

/// The R7 starts of a held-out session by workload and by loaded CCD. Every R7 start counts, y-cruncher
/// included. Single-core loads are also counted in `singleton`, besides their workload and CCD rows: they
/// are predicted by the per-core mechanisms, not the shared voltage.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct R7Rows {
    pub workloads: BTreeMap<WorkloadId, ForwardCounts>,
    pub ccds: BTreeMap<R7Ccd, ForwardCounts>,
    pub singleton: ForwardCounts,
}

/// One scored held-out trial, identified by its session and journal sequence number, with the exact values
/// its row's counts sum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrialScore {
    pub session: SessionId,
    pub seq: Seq,
    pub workload: WorkloadId,
    /// How many cores the trial loaded.
    pub loaded: usize,
    pub failure: bool,
    /// The predicted failure probability, before the log-loss clamp.
    pub probability: f64,
    pub fit_log_loss: f64,
    pub constant_log_loss: f64,
    /// A failure the prediction gave less than 1% probability.
    pub surprise: bool,
    /// The trial's class and full profile occur in the earlier sessions.
    pub matched: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Breakdown {
    pub regimes: BTreeMap<Regime, ForwardCounts>,
    pub r7: R7Rows,
    pub trials: Vec<TrialScore>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ForwardRow {
    pub session: SessionId,
    pub ruleset: u32,
    pub training_sessions: usize,
    /// The earlier trials' failure rate: the constant predictor's probability.
    pub constant: f64,
    pub score: ForwardScore,
    pub regimes: BTreeMap<Regime, ForwardCounts>,
    pub r7: R7Rows,
    /// The scored trials in the held-out session's order.
    pub trials: Vec<TrialScore>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ForwardCheck {
    pub rows: Vec<ForwardRow>,
    pub pooled: ForwardScore,
}

pub(crate) fn log_loss(p: f64, failure: bool) -> f64 {
    let p = p.clamp(CLAMP, 1.0 - CLAMP);
    if failure { -p.ln() } else { -(-p).ln_1p() }
}

/// The trials in session order and the index where each session begins, without the newest `seal` sessions.
fn segment<'a>(
    trials: impl IntoIterator<Item = &'a TrialFact>,
    seal: usize,
) -> Result<(Vec<&'a TrialFact>, Vec<usize>), Error> {
    let mut ordered: Vec<&TrialFact> = trials.into_iter().collect();
    ordered.sort_by_key(|fact| fact.session);
    let mut bounds: Vec<usize> = Vec::new();
    for i in 0..ordered.len() {
        if i == 0 || ordered[i].session != ordered[i - 1].session {
            bounds.push(i);
        }
    }
    if seal > 0 {
        let held_out = bounds.len().saturating_sub(1);
        if seal >= held_out {
            return Err(Error::Seal { seal, held_out });
        }
        ordered.truncate(bounds[bounds.len() - seal]);
        bounds.truncate(bounds.len() - seal);
    }
    Ok((ordered, bounds))
}

/// Runs the check over decisive `trials`; the newest `seal` sessions are neither fitted nor scored. The
/// result does not depend on the thread count: each prefix fit lands in its own slot, and scoring and
/// pooling run in session order.
pub fn forward_check<'a>(
    trials: impl IntoIterator<Item = &'a TrialFact>,
    seal: usize,
) -> Result<ForwardCheck, Error> {
    let (ordered, bounds) = segment(trials, seal)?;
    let fits: Vec<Result<MachineDefinition, Error>> = (0..bounds.len().saturating_sub(1))
        .into_par_iter()
        .map(|i| fit(&ordered[..bounds[i + 1]]).map(|(cfg, _)| cfg))
        .collect();
    let configs = fits.into_iter().collect::<Result<Vec<_>, _>>()?;
    chain(&ordered, |training_sessions, held_out, seen, constant| {
        score_forward(&configs[training_sessions - 1], held_out, seen, constant)
    })
}

/// Scores the held-out sessions of [`forward_check`] with the fixed ensemble `members` instead of the prefix
/// fits, fitting nothing. The rows, constants and seal are those of the forward check on the same `trials`.
///
/// Convention: the probability of a trial is the mean over all members of the member's failure probability
/// for it, taken member by member in the order given; the log loss, the predicted failures and the 1%
/// surprise test use that mean. A model-check group counts as flagged when any member flags it.
pub fn reference_check<'a>(
    trials: impl IntoIterator<Item = &'a TrialFact>,
    seal: usize,
    members: &[MachineDefinition],
) -> Result<ForwardCheck, Error> {
    if members.is_empty() {
        return Err(Error::Report(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the reference ensemble has no member",
        )));
    }
    let (ordered, _) = segment(trials, seal)?;
    let models = members
        .iter()
        .map(|cfg| Model::new(cfg, 0))
        .collect::<Result<Vec<_>, _>>()?;
    chain(&ordered, |_, held_out, seen, constant| {
        score_reference(&models, held_out, seen, constant)
    })
}

/// Scores every session after the first with `score` and pools the rows in session order. `score` gets the
/// number of sessions before the held-out one, the held-out trials, the earlier trials' profile classes and
/// the constant predictor's probability.
fn chain(
    ordered: &[&TrialFact],
    mut score: impl FnMut(
        usize,
        &[&TrialFact],
        &HashSet<ProfileClassKey>,
        f64,
    ) -> Result<(ForwardScore, Breakdown), Error>,
) -> Result<ForwardCheck, Error> {
    let mut seen: HashSet<ProfileClassKey> = HashSet::new();
    let (mut training_failures, mut training_sessions) = (0, 0);
    let mut rows = Vec::new();
    let mut pooled = ForwardScore::default();
    let mut begin = 0;
    while begin < ordered.len() {
        let mut end = begin + 1;
        while end < ordered.len() && ordered[end].session == ordered[begin].session {
            end += 1;
        }
        let held_out = &ordered[begin..end];
        if begin > 0 {
            let constant = saturating_count(training_failures) / saturating_count(begin);
            let (scored, breakdown) = score(training_sessions, held_out, &seen, constant)?;
            rows.push(ForwardRow {
                session: held_out[0].session,
                ruleset: held_out[0].build.ruleset,
                training_sessions,
                constant,
                score: scored,
                regimes: breakdown.regimes,
                r7: breakdown.r7,
                trials: breakdown.trials,
            });
            let c = &scored.counts;
            pooled.counts.trials += c.trials;
            pooled.counts.failures += c.failures;
            pooled.counts.predicted += c.predicted;
            pooled.counts.fit_log_loss += c.fit_log_loss;
            pooled.counts.constant_log_loss += c.constant_log_loss;
            pooled.surprises += scored.surprises;
            pooled.matches += scored.matches;
            pooled.flagged += scored.flagged;
            pooled.eligible += scored.eligible;
        }
        for &fact in held_out {
            seen.insert(ProfileClassKey::from(fact));
            if is_failure(fact) {
                training_failures += 1;
            }
        }
        training_sessions += 1;
        begin = end;
    }
    Ok(ForwardCheck { rows, pooled })
}

pub(crate) fn score_forward(
    cfg: &MachineDefinition,
    held_out: &[&TrialFact],
    seen: &HashSet<ProfileClassKey>,
    constant: f64,
) -> Result<(ForwardScore, Breakdown), Error> {
    let model = Model::new(cfg, 0)?;
    let checker = Checker::new(&model, held_out.iter().copied())?;
    let groups = checker.groups(&model)?;
    let (mut score, breakdown) = score_trials(held_out, seen, constant, |fact, class| {
        Ok(model.failure_probability(fact.profile.as_slice(), &load_of(class))?)
    })?;
    score.eligible = groups.len();
    score.flagged = groups.iter().filter(|g| g.flagged).count();
    Ok((score, breakdown))
}

fn score_reference(
    models: &[Model],
    held_out: &[&TrialFact],
    seen: &HashSet<ProfileClassKey>,
    constant: f64,
) -> Result<(ForwardScore, Breakdown), Error> {
    let mut eligible: HashSet<String> = HashSet::new();
    let mut flagged: HashSet<String> = HashSet::new();
    for model in models {
        let checker = Checker::new(model, held_out.iter().copied())?;
        for group in checker.groups(model)? {
            let key = group.to_string();
            if group.flagged {
                flagged.insert(key.clone());
            }
            eligible.insert(key);
        }
    }
    let members = saturating_count(models.len());
    let (mut score, breakdown) = score_trials(held_out, seen, constant, |fact, class| {
        let mut sum = 0.0;
        for model in models {
            sum += model.failure_probability(fact.profile.as_slice(), &load_of(class))?;
        }
        Ok(sum / members)
    })?;
    score.eligible = eligible.len();
    score.flagged = flagged.len();
    Ok((score, breakdown))
}

fn score_trials(
    held_out: &[&TrialFact],
    seen: &HashSet<ProfileClassKey>,
    constant: f64,
    mut probability: impl FnMut(&TrialFact, &togi_facts::TrialClass) -> Result<f64, Error>,
) -> Result<(ForwardScore, Breakdown), Error> {
    let mut score = ForwardScore::default();
    let mut breakdown = Breakdown::default();
    for &fact in held_out {
        let Some(class) = trial_class(fact) else {
            continue;
        };
        let p = probability(fact, &class)?;
        let failure = is_failure(fact);
        let regime = regime_of(fact);
        let fit_log_loss = log_loss(p, failure);
        let constant_log_loss = log_loss(constant, failure);
        let add = |counts: &mut ForwardCounts| {
            counts.trials += 1;
            counts.predicted += p;
            counts.fit_log_loss += fit_log_loss;
            counts.constant_log_loss += constant_log_loss;
            if failure {
                counts.failures += 1;
            }
        };
        add(&mut score.counts);
        add(breakdown.regimes.entry(regime).or_default());
        if regime == Regime::R7 {
            add(breakdown.r7.workloads.entry(class.workload).or_default());
            add(breakdown.r7.ccds.entry(r7_ccd(fact, class)).or_default());
            if class.loaded.len() == 1 {
                add(&mut breakdown.r7.singleton);
            }
        }
        let surprise = failure && p < 0.01;
        if surprise {
            score.surprises += 1;
        }
        let matched = seen.contains(&ProfileClassKey::from(fact));
        if matched {
            score.matches += 1;
        }
        breakdown.trials.push(TrialScore {
            session: fact.session,
            seq: fact.seq,
            workload: class.workload,
            loaded: class.loaded.len(),
            failure,
            probability: p,
            fit_log_loss,
            constant_log_loss,
            surprise,
            matched,
        });
    }
    Ok((score, breakdown))
}

fn r7_ccd(fact: &TrialFact, class: togi_facts::TrialClass) -> R7Ccd {
    let half = (fact.profile.as_slice().len() / 2).max(1);
    let first = class.loaded.iter().any(|core| core.index() < half);
    let second = class.loaded.iter().any(|core| core.index() >= half);
    match (first, second) {
        (true, true) => R7Ccd::Both,
        (false, true) => R7Ccd::Ccd1,
        (_, false) => R7Ccd::Ccd0,
    }
}

pub fn report_forward(
    out: &mut impl io::Write,
    check: &ForwardCheck,
    seal: usize,
) -> io::Result<()> {
    writeln!(out, "\nForward-chained check")?;
    report_rows(out, check, seal, "flagged/eligible")
}

/// Reports a [`reference_check`] of `members` ensemble members, with the convention that gives its numbers.
pub fn report_reference(
    out: &mut impl io::Write,
    check: &ForwardCheck,
    seal: usize,
    members: usize,
) -> io::Result<()> {
    writeln!(
        out,
        "\nReference ensemble score on the same sessions ({members} members, committed, not fitted, not a gate)"
    )?;
    writeln!(
        out,
        "Convention: each trial's probability is the mean over all {members} members of the member's failure probability; fit loss, predicted failures and failures_p<0.01 use that mean; flagged counts model-check groups flagged by any member; constant as in the forward check."
    )?;
    report_rows(out, check, seal, "flagged_by_any_member/eligible")
}

fn report_rows(
    out: &mut impl io::Write,
    check: &ForwardCheck,
    seal: usize,
    flagged: &str,
) -> io::Result<()> {
    if check.rows.is_empty() {
        return writeln!(
            out,
            "No held-out sessions (need at least two sessions with decisive trials)."
        );
    }
    for row in &check.rows {
        write!(
            out,
            "{} ruleset={} training_sessions={}: ",
            row.session, row.ruleset, row.training_sessions
        )?;
        report_score(out, &row.score, &format!("{:.3}", row.constant), flagged)?;
        for regime in REGIMES {
            if let Some(c) = row.regimes.get(&regime) {
                report_counts(out, &regime.to_string(), c)?;
            }
        }
        for (workload, c) in &row.r7.workloads {
            report_counts(out, &format!("R7 workload {}", workload.as_str()), c)?;
        }
        for (ccd, c) in &row.r7.ccds {
            report_counts(out, ccd.label(), c)?;
        }
        if row.r7.singleton.trials > 0 {
            report_counts(out, "R7 singleton (also in its CCD row)", &row.r7.singleton)?;
        }
    }
    if seal > 0 {
        writeln!(
            out,
            "Sealed newest sessions: {seal} (neither fitted nor scored)"
        )?;
    }
    write!(out, "Pooled: ")?;
    report_score(out, &check.pooled, "per-prefix", flagged)
}

fn report_counts(out: &mut impl io::Write, label: &str, c: &ForwardCounts) -> io::Result<()> {
    let n = saturating_count(c.trials);
    writeln!(
        out,
        "  {label} trials={} observed={} predicted={:.1} log_loss/trial fit={:.4} constant={:.4}",
        c.trials,
        c.failures,
        c.predicted,
        c.fit_log_loss / n,
        c.constant_log_loss / n
    )
}

fn report_score(
    out: &mut impl io::Write,
    score: &ForwardScore,
    constant: &str,
    flagged: &str,
) -> io::Result<()> {
    let c = &score.counts;
    let n = saturating_count(c.trials);
    writeln!(
        out,
        "trials={} failures={} predicted={:.1} log_loss/trial fit={:.4} constant({constant})={:.4} failures_p<0.01={} exact_matches={}/{} ({:.1}%) {flagged}={}/{}",
        c.trials,
        c.failures,
        c.predicted,
        c.fit_log_loss / n,
        c.constant_log_loss / n,
        score.surprises,
        score.matches,
        c.trials,
        100.0 * saturating_count(score.matches) / n,
        score.flagged,
        score.eligible
    )
}
