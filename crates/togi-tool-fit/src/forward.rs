//! The forward-chained check: for each real session after the first, fit the earlier sessions' decisive
//! trials and score the held-out session, so no outcome informs its own prediction.

use std::collections::{BTreeMap, HashSet};
use std::io;

use rayon::prelude::*;
use togi_journal::SessionId;
use togi_machine::{REGIMES, Regime};
use togi_sim::{MachineDefinition, Model};
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
    pub flagged: usize,
    pub eligible: usize,
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

fn count(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Runs the check over decisive `trials`; the newest `seal` sessions are neither fitted nor scored. The
/// result does not depend on the thread count: each prefix fit lands in its own slot, and scoring and
/// pooling run in session order.
pub fn forward_check(trials: &[TrialFact], seal: usize) -> Result<ForwardCheck, Error> {
    let mut ordered = trials.to_vec();
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
    let fits: Vec<Result<MachineDefinition, Error>> = (0..bounds.len().saturating_sub(1))
        .into_par_iter()
        .map(|i| fit(&ordered[..bounds[i + 1]]).map(|(cfg, _)| cfg))
        .collect();
    let configs = fits.into_iter().collect::<Result<Vec<_>, _>>()?;
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
            let constant = count(training_failures) / count(begin);
            let (score, regimes) =
                score_forward(&configs[training_sessions - 1], held_out, &seen, constant)?;
            rows.push(ForwardRow {
                session: held_out[0].session,
                ruleset: held_out[0].build.ruleset,
                training_sessions,
                constant,
                score,
                regimes,
            });
            let c = &score.counts;
            pooled.counts.trials += c.trials;
            pooled.counts.failures += c.failures;
            pooled.counts.predicted += c.predicted;
            pooled.counts.fit_log_loss += c.fit_log_loss;
            pooled.counts.constant_log_loss += c.constant_log_loss;
            pooled.surprises += score.surprises;
            pooled.matches += score.matches;
            pooled.flagged += score.flagged;
            pooled.eligible += score.eligible;
        }
        for fact in held_out {
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
    held_out: &[TrialFact],
    seen: &HashSet<ProfileClassKey>,
    constant: f64,
) -> Result<(ForwardScore, BTreeMap<Regime, ForwardCounts>), Error> {
    let model = Model::new(cfg, 0)?;
    let checker = Checker::new(&model, held_out)?;
    let groups = checker.groups(&model)?;
    let mut score = ForwardScore {
        eligible: groups.len(),
        flagged: groups.iter().filter(|g| g.flagged).count(),
        ..ForwardScore::default()
    };
    let mut regimes: BTreeMap<Regime, ForwardCounts> = BTreeMap::new();
    for fact in held_out {
        let Some(class) = trial_class(fact) else {
            continue;
        };
        let p = model.failure_probability(fact.profile.as_slice(), &load_of(&class))?;
        let failure = is_failure(fact);
        let counts = regimes.entry(regime_of(fact)).or_default();
        for counts in [counts, &mut score.counts] {
            counts.trials += 1;
            counts.predicted += p;
            counts.fit_log_loss += log_loss(p, failure);
            counts.constant_log_loss += log_loss(constant, failure);
            if failure {
                counts.failures += 1;
            }
        }
        if failure && p < 0.01 {
            score.surprises += 1;
        }
        if seen.contains(&ProfileClassKey::from(fact)) {
            score.matches += 1;
        }
    }
    Ok((score, regimes))
}

pub fn report_forward(
    out: &mut impl io::Write,
    check: &ForwardCheck,
    seal: usize,
) -> io::Result<()> {
    writeln!(out, "\nForward-chained check")?;
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
        report_score(out, &row.score, &format!("{:.3}", row.constant))?;
        for regime in REGIMES {
            if let Some(c) = row.regimes.get(&regime) {
                let n = count(c.trials);
                writeln!(
                    out,
                    "  {regime} trials={} observed={} predicted={:.1} log_loss/trial fit={:.4} constant={:.4}",
                    c.trials,
                    c.failures,
                    c.predicted,
                    c.fit_log_loss / n,
                    c.constant_log_loss / n
                )?;
            }
        }
    }
    if seal > 0 {
        writeln!(
            out,
            "Sealed newest sessions: {seal} (neither fitted nor scored)"
        )?;
    }
    write!(out, "Pooled: ")?;
    report_score(out, &check.pooled, "per-prefix")
}

fn report_score(out: &mut impl io::Write, score: &ForwardScore, constant: &str) -> io::Result<()> {
    let c = &score.counts;
    let n = count(c.trials);
    writeln!(
        out,
        "trials={} failures={} predicted={:.1} log_loss/trial fit={:.4} constant({constant})={:.4} failures_p<0.01={} exact_matches={}/{} ({:.1}%) flagged/eligible={}/{}",
        c.trials,
        c.failures,
        c.predicted,
        c.fit_log_loss / n,
        c.constant_log_loss / n,
        score.surprises,
        score.matches,
        c.trials,
        100.0 * count(score.matches) / n,
        score.flagged,
        score.eligible
    )
}
