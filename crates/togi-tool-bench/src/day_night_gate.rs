//! Policy selection and the gate verdict of the day/night experiment: which policies run, what
//! each scenario measured against its registered threshold, and whether the run may fail a gate.

use std::io::{self, Write};
use std::time::Duration;

use togi_sim::saturating_count;
use togi_simrun::day_night::{PolicyError, PolicyKind, PolicyRole};

use crate::day_night::Registration;
use crate::day_night_report::{Criteria, Measured, Record, ScoreError, measure, score};

#[derive(Debug, thiserror::Error)]
pub enum SelectError {
    #[error("select a day/night policy")]
    Policy(#[from] PolicyError),
    #[error("fallback slope {slope} is not one of the registered slopes {registered:?}")]
    SlopeNotRegistered { slope: f64, registered: Vec<f64> },
    #[error("policy {0} was selected twice")]
    Duplicate(&'static str),
}

/// The policies to run, in the order given. No names selects every diagnostic comparator.
/// A slope must be one the registration lists, and only a policy that takes one accepts it.
///
/// # Errors
/// Refuses an unknown name, a duplicate, a slope for a policy without one, and an unregistered slope.
pub fn select_policies(
    registration: &Registration,
    names: &[String],
    slope: Option<f64>,
) -> Result<Vec<PolicyKind>, SelectError> {
    if let Some(slope) = slope
        && !registration
            .policy_slopes
            .iter()
            .any(|registered| registered.to_bits() == slope.to_bits())
    {
        return Err(SelectError::SlopeNotRegistered {
            slope,
            registered: registration.policy_slopes.clone(),
        });
    }
    let defaults = PolicyKind::COMPARATORS.map(|policy| policy.label().to_owned());
    let names = if names.is_empty() {
        &defaults[..]
    } else {
        names
    };
    let mut policies: Vec<PolicyKind> = Vec::with_capacity(names.len());
    for name in names {
        let policy = PolicyKind::by_label(name, slope)?;
        if policies
            .iter()
            .any(|chosen| chosen.label() == policy.label())
        {
            return Err(SelectError::Duplicate(policy.label()));
        }
        policies.push(policy);
    }
    Ok(policies)
}

/// One scenario's measurements and the registered criteria applied to them.
#[derive(Debug)]
pub struct ScenarioVerdict {
    pub scenario: String,
    pub criteria: Criteria,
    pub measured: Measured,
}

/// Every registered scenario scored for one policy.
#[derive(Debug)]
pub struct Verdict {
    pub policy: PolicyKind,
    /// Whether this verdict may fail a gate; the policy's own role when computed.
    pub role: PolicyRole,
    pub scenarios: Vec<ScenarioVerdict>,
}

impl Verdict {
    /// Scores the policy's records with the registered criteria.
    ///
    /// # Errors
    /// Refuses incomplete seed sets and malformed metrics; see [`score`].
    pub fn compute(
        registration: &Registration,
        selection: crate::Selection,
        policy: PolicyKind,
        records: &[Record],
    ) -> Result<Self, ScoreError> {
        let scenarios = score(registration, selection, policy.label(), records)?
            .into_iter()
            .map(|(scenario, criteria)| {
                let metrics: Vec<_> = records
                    .iter()
                    .filter(|record| {
                        record.scenario == scenario && record.comparator == policy.label()
                    })
                    .map(|record| &record.metrics)
                    .collect();
                ScenarioVerdict {
                    measured: measure(registration, &metrics),
                    scenario,
                    criteria,
                }
            })
            .collect();
        Ok(Self {
            policy,
            role: policy.role(),
            scenarios,
        })
    }

    pub fn passed(&self) -> bool {
        self.scenarios
            .iter()
            .all(|scenario| scenario.criteria.passed())
    }

    /// Whether this verdict fails a gate: only the policy under test can, never a comparator.
    pub fn fails_gate(&self) -> bool {
        match self.role {
            PolicyRole::Gate => !self.passed(),
            PolicyRole::Diagnostic => false,
        }
    }
}

/// Whether any verdict fails the gate, which makes the command exit nonzero.
pub fn any_fails_gate(verdicts: &[Verdict]) -> bool {
    verdicts.iter().any(Verdict::fails_gate)
}

fn cell(ok: Option<bool>, measured: &str, threshold: &str, relation: &str) -> String {
    match ok {
        None => "—".to_owned(),
        Some(true) => format!("{measured} {relation} {threshold}"),
        Some(false) => format!("{measured} {relation} {threshold} ✗"),
    }
}

fn heading(policy: PolicyKind, role: PolicyRole) -> String {
    let slope = policy
        .fallback_slope()
        .map_or_else(String::new, |slope| format!(" (fallback slope {slope}×)"));
    match role {
        PolicyRole::Gate => format!("{}{slope}", policy.label()),
        PolicyRole::Diagnostic => {
            format!("{}{slope}: diagnostic (not a gate pass)", policy.label())
        }
    }
}

/// Renders the Markdown verdict table and the overall line for one policy. Every cell is the
/// measured value, the relation the registration demands and its threshold; a failing cell ends in
/// `✗` and `—` marks a criterion the scenario does not have.
pub fn render_verdict(
    out: &mut dyn Write,
    registration: &Registration,
    verdict: &Verdict,
) -> io::Result<()> {
    let thresholds = &registration.thresholds;
    writeln!(out, "### {}", heading(verdict.policy, verdict.role))?;
    writeln!(out)?;
    writeln!(
        out,
        "| scenario | climb-free | year-one | overshoot ≤1 | overshoot max | median convergence d | retained | max steps | censored | result |"
    )?;
    writeln!(out, "|---|---|---|---|---|---|---|---|---|---|")?;
    for row in &verdict.scenarios {
        let (criteria, measured) = (&row.criteria, &row.measured);
        let cells = [
            cell(
                criteria.covered_climb,
                &format!("{:.3}", measured.climb_free),
                &format!("{:.2}", thresholds.covered_crash_free_fraction),
                "≥",
            ),
            cell(
                criteria.hidden_year_one,
                &format!("{:.3}", measured.year_one_within),
                &format!("{:.2}", thresholds.hidden_year_one_fraction),
                "≥",
            ),
            cell(
                Some(criteria.overshoot_fraction),
                &format!("{:.3}", measured.overshoot_one),
                &format!("{:.2}", thresholds.overshoot_one_count_fraction),
                "≥",
            ),
            cell(
                Some(criteria.overshoot_absolute),
                &measured.max_overshoot.to_string(),
                &thresholds.overshoot_absolute_max_counts.to_string(),
                "≤",
            ),
            cell(
                Some(criteria.convergence),
                &format!("{:.1}", measured.median_convergence_days),
                &thresholds.median_convergence_days.to_string(),
                "≤",
            ),
            cell(
                criteria.retention,
                &format!("{:.3}", measured.retained),
                &format!("{:.2}", thresholds.retention_fraction),
                "≥",
            ),
            cell(
                criteria.fixed_cap_steps,
                &measured.max_promotions.to_string(),
                &thresholds.fixed_cap_max_steps_per_ccd.to_string(),
                "≤",
            ),
        ];
        writeln!(
            out,
            "| {} | {} | {}/{} | {} |",
            row.scenario,
            cells.join(" | "),
            measured.censored,
            measured.runs,
            if criteria.passed() { "PASS" } else { "FAIL" }
        )?;
    }
    writeln!(out)?;
    let overall = if verdict.passed() { "PASS" } else { "FAIL" };
    match verdict.role {
        PolicyRole::Gate => writeln!(out, "{}: {overall}", registration.id),
        PolicyRole::Diagnostic => writeln!(
            out,
            "{}: {overall} ({}, diagnostic, not a gate pass)",
            registration.id,
            verdict.policy.label()
        ),
    }
}

/// Mean night hours and mean wall time per scenario and policy, then the run's total wall time.
pub fn render_summary(
    out: &mut dyn Write,
    registration: &Registration,
    policies: &[PolicyKind],
    records: &[Record],
    total: Duration,
) -> io::Result<()> {
    writeln!(
        out,
        "| scenario | policy | runs | mean night hours | mean run wall s |"
    )?;
    writeln!(out, "|---|---|---|---|---|")?;
    for scenario in &registration.scenarios {
        for policy in policies {
            let selected: Vec<_> = records
                .iter()
                .filter(|record| {
                    record.scenario == scenario.name && record.comparator == policy.label()
                })
                .collect();
            let runs = saturating_count(selected.len());
            writeln!(
                out,
                "| {} | {} | {} | {:.1} | {:.2} |",
                scenario.name,
                policy.label(),
                selected.len(),
                selected
                    .iter()
                    .map(|record| record.metrics.night_hours)
                    .sum::<f64>()
                    / runs,
                selected.iter().map(|record| record.wall_s).sum::<f64>() / runs,
            )?;
        }
    }
    writeln!(out)?;
    writeln!(out, "total wall time: {:.1} s", total.as_secs_f64())
}
