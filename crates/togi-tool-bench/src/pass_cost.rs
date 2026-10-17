//! The offline pass and cost calculator of #490: what the checking cycle's R7 part requirements cost at a fixed
//! profile under a failure probability per trial, from constant hazards or a simulator machine.
//!
//! Every number is computed, never sampled, and nothing here reaches the tuner: no journal is read or written and
//! no result becomes evidence. The first-attempt probability is the chance that a part's required passes all pass
//! in a row at the fixed profile, the exact product of `1 - p` over its trials. Everything else is
//! **counterfactual**: retrying the requirement at the same profile until it is met, with a failure resetting only
//! its own class's passes, as the evidence ledger invalidates them, where the tuner would instead back off; and
//! requirements with more short passes than the tuner asks for.

use std::io;
use std::num::NonZeroU32;
use std::time::Duration;

use togi_config::Config;
use togi_machine::{CoreId, CoreSet, Regime, TargetOffset, workloads};
use togi_sim::{Load, Model, ShapeError};
use togi_tuner::{R7PartRole, Secs, r7_part_passes};

use crate::metrics::{CORES_PER_CCD, next_partial};

/// The constant hazards of the #490 readiness grid, failures per hour.
pub const GRID_PER_HOUR: [f64; 8] = [0.0, 0.1, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];

/// Passes of one trial class the requirement needs, each trial failing with probability `failure`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Requirement {
    pub trial: Secs,
    pub count: u32,
    pub failure: f64,
}

/// Why a requirement cannot be evaluated.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ObligationError {
    #[error("an obligation needs at least one requirement")]
    Empty,
    #[error("a requirement needs at least one pass")]
    NoPasses,
    #[error("failure probability {0} is not within 0 to 1")]
    Probability(f64),
}

/// Requirements met in order, the first unmet one running next. A failure resets the passes of its own
/// requirement only; the requirements before it are distinct classes and keep theirs.
#[derive(Clone, Debug, PartialEq)]
pub struct Obligation {
    requirements: Vec<Requirement>,
}

/// What a finite horizon leaves: the probability the obligation is met within it and the expected launches or
/// seconds spent before it is met or the horizon ends. The censored share is `1 - done`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Horizon {
    pub done: f64,
    pub spent: f64,
}

impl Obligation {
    pub fn new(requirements: Vec<Requirement>) -> Result<Self, ObligationError> {
        if requirements.is_empty() {
            return Err(ObligationError::Empty);
        }
        for requirement in &requirements {
            if requirement.count == 0 {
                return Err(ObligationError::NoPasses);
            }
            if !(0.0..=1.0).contains(&requirement.failure) {
                return Err(ObligationError::Probability(requirement.failure));
            }
        }
        Ok(Self { requirements })
    }

    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// The obligations of `parts` in turn, `None` for no parts: their classes differ, so a failure in one leaves
    /// the others' passes.
    pub fn chain(parts: &[Self]) -> Option<Self> {
        let requirements: Vec<Requirement> = parts
            .iter()
            .flat_map(|part| part.requirements.iter().copied())
            .collect();
        (!requirements.is_empty()).then_some(Self { requirements })
    }

    /// The probability that every required trial passes on its first launch: `Π (1 - p)^count`.
    pub fn first_attempt(&self) -> f64 {
        self.requirements
            .iter()
            .map(|r| (1.0 - r.failure).powf(f64::from(r.count)))
            .product()
    }

    /// Expected launches to meet the obligation by retrying: `Σ_{i=1..count} (1 - p)^-i` per requirement, the
    /// waiting time for `count` passes in a row. Infinite when a trial always fails.
    pub fn expected_launches(&self) -> f64 {
        self.requirements.iter().map(streak_launches).sum()
    }

    /// Expected seconds to meet the obligation by retrying, each launch charged its full intended duration.
    pub fn expected_seconds(&self) -> f64 {
        self.requirements
            .iter()
            .map(|r| streak_launches(r) * f64::from(r.trial.get()))
            .sum()
    }

    /// The obligation within `limit` launches, by dynamic programming over the passes earned so far.
    pub fn within_launches(&self, limit: u32) -> Horizon {
        let states = self.states();
        let mut mass = vec![0.0; states.len()];
        mass[0] = 1.0;
        let mut horizon = Horizon {
            done: 0.0,
            spent: 0.0,
        };
        for _ in 0..limit {
            horizon.spent += mass.iter().sum::<f64>();
            let mut next = vec![0.0; states.len()];
            for (at, state) in states.iter().enumerate() {
                let m = mass[at];
                if m == 0.0 {
                    continue;
                }
                let failure = self.requirements[state.requirement].failure;
                match next.get_mut(at + 1) {
                    Some(slot) => *slot += m * (1.0 - failure),
                    None => horizon.done += m * (1.0 - failure),
                }
                next[state.restart] += m * failure;
            }
            mass = next;
        }
        horizon
    }

    /// The obligation within `limit` seconds, by dynamic programming over the passes earned and the seconds
    /// elapsed, each launch charged its full intended duration. A launch that would end after the limit is
    /// censored; its seconds up to the limit count as spent.
    pub fn within_seconds(&self, limit: u32) -> Horizon {
        let states = self.states();
        let longest = self
            .requirements
            .iter()
            .map(|r| r.trial.get())
            .max()
            .unwrap_or(1);
        let width = longest as usize + 1;
        let mut ring = vec![vec![0.0; states.len()]; width];
        ring[0][0] = 1.0;
        let mut horizon = Horizon {
            done: 0.0,
            spent: 0.0,
        };
        for now in 0..limit {
            let mass = std::mem::replace(&mut ring[now as usize % width], vec![0.0; states.len()]);
            for (at, state) in states.iter().enumerate() {
                let m = mass[at];
                if m == 0.0 {
                    continue;
                }
                let requirement = self.requirements[state.requirement];
                let seconds = requirement.trial.get();
                horizon.spent += m * f64::from(seconds.min(limit - now));
                let Some(end) = now.checked_add(seconds).filter(|&end| end <= limit) else {
                    continue;
                };
                let slot = &mut ring[end as usize % width];
                match slot.get_mut(at + 1) {
                    Some(next) => *next += m * (1.0 - requirement.failure),
                    None => horizon.done += m * (1.0 - requirement.failure),
                }
                slot[state.restart] += m * requirement.failure;
            }
        }
        horizon
    }

    /// One state per pass still to earn, in order: its requirement and the state its failure returns to.
    fn states(&self) -> Vec<State> {
        let mut states = Vec::new();
        for (requirement, r) in self.requirements.iter().enumerate() {
            let restart = states.len();
            for _ in 0..r.count {
                states.push(State {
                    requirement,
                    restart,
                });
            }
        }
        states
    }
}

#[derive(Clone, Copy, Debug)]
struct State {
    requirement: usize,
    restart: usize,
}

fn streak_launches(requirement: &Requirement) -> f64 {
    let pass = 1.0 - requirement.failure;
    (1..=requirement.count)
        .map(|i| pass.powf(-f64::from(i)))
        .sum()
}

/// The probability that a trial of `seconds` fails under a constant hazard of `per_hour` failures per hour.
pub fn constant_failure(per_hour: f64, seconds: Secs) -> f64 {
    -(-per_hour * f64::from(seconds.get()) / 3600.0).exp_m1()
}

/// One R7 part: what it loads and its requirement.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub role: R7PartRole,
    pub cores: Option<CoreSet>,
    pub obligation: Obligation,
}

/// The parts of one source of failure probabilities: a constant hazard, or one R7 workload's step on a machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub source: Source,
    pub parts: Vec<Part>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Hazard { per_hour: f64 },
    Step { workload: &'static str },
}

/// The CCD part and, on several CCDs, the all-core part at each constant hazard of `per_hour`, on a machine of
/// `ccds` CCDs. A partial's requirement is the CCD part's.
pub fn hazard_blocks(
    config: &Config,
    ccds: usize,
    per_hour: &[f64],
) -> Result<Vec<Block>, ObligationError> {
    let mut roles = vec![R7PartRole::Ccd];
    if ccds > 1 {
        roles.push(R7PartRole::AllCore);
    }
    per_hour
        .iter()
        .map(|&rate| {
            let parts = roles
                .iter()
                .map(|&role| {
                    let obligation = Obligation::new(
                        r7_part_passes(config, ccds, role)
                            .into_iter()
                            .map(|(trial, count)| Requirement {
                                trial,
                                count,
                                failure: constant_failure(rate, trial),
                            })
                            .collect(),
                    )?;
                    Ok(Part {
                        role,
                        cores: None,
                        obligation,
                    })
                })
                .collect::<Result<_, ObligationError>>()?;
            Ok(Block {
                source: Source::Hazard { per_hour: rate },
                parts,
            })
        })
        .collect()
}

/// Why a machine's parts cannot be evaluated.
#[derive(Debug, thiserror::Error)]
pub enum MachineError {
    #[error("evaluate the machine model")]
    Shape(#[from] ShapeError),
    #[error("evaluate a requirement")]
    Obligation(#[from] ObligationError),
    #[error("an R7 part loads at least two cores")]
    SingleCore,
    #[error("unknown R7 workload {0}")]
    Workload(String),
}

/// What a machine study evaluates: the parts of each selected R7 workload's step at `profile`, or only the part
/// that loads `cores`.
#[derive(Clone, Debug)]
pub struct MachineQuery<'a> {
    pub profile: &'a [TargetOffset],
    pub workload: Option<&'a str>,
    pub cores: Option<CoreSet>,
}

/// Each selected R7 workload's step on `model` at the query's profile: every CCD's full part, then its
/// request-ordered partials of at least two cores when the machine has a shared rail, then the all-core part on
/// several CCDs; or only the part that loads the query's cores. Each trial's failure probability is the model's,
/// over its intended duration, background included.
pub fn machine_blocks(
    config: &Config,
    model: &Model,
    query: &MachineQuery<'_>,
) -> Result<Vec<Block>, MachineError> {
    let catalog = workloads(Regime::R7);
    let selected: Vec<&'static str> = match query.workload {
        Some(id) => vec![
            catalog
                .iter()
                .find(|w| w.id == id)
                .ok_or_else(|| MachineError::Workload(id.to_owned()))?
                .id,
        ],
        None => catalog.iter().map(|w| w.id).collect(),
    };
    let ccds = model.cores().div_ceil(CORES_PER_CCD);
    let all: CoreSet = (0..model.cores()).filter_map(core).collect();
    selected
        .into_iter()
        .map(|workload| {
            let loads = match query.cores {
                Some(cores) if cores.len() < 2 => return Err(MachineError::SingleCore),
                Some(cores) => vec![(role_of(cores, all, ccds), cores)],
                None => step_loads(model, query.profile, workload, ccds, all)?,
            };
            let parts = loads
                .into_iter()
                .map(|(role, cores)| {
                    let requirements = r7_part_passes(config, ccds, role)
                        .into_iter()
                        .map(|(trial, count)| {
                            let load = Load {
                                regime: Regime::R7,
                                workload,
                                cores,
                                duration: Duration::from_secs(u64::from(trial.get())),
                            };
                            Ok(Requirement {
                                trial,
                                count,
                                failure: model.failure_probability(query.profile, &load)?,
                            })
                        })
                        .collect::<Result<_, MachineError>>()?;
                    Ok(Part {
                        role,
                        cores: Some(cores),
                        obligation: Obligation::new(requirements)?,
                    })
                })
                .collect::<Result<_, MachineError>>()?;
            Ok(Block {
                source: Source::Step { workload },
                parts,
            })
        })
        .collect()
}

fn role_of(cores: CoreSet, all: CoreSet, ccds: usize) -> R7PartRole {
    if ccds > 1 && cores == all {
        return R7PartRole::AllCore;
    }
    let full = (0..ccds).any(|ccd| ccd_cores(ccd, all) == cores);
    if full {
        R7PartRole::Ccd
    } else {
        R7PartRole::Partial
    }
}

fn ccd_cores(ccd: usize, all: CoreSet) -> CoreSet {
    all.iter()
        .filter(|core| core.index() / CORES_PER_CCD == ccd)
        .collect()
}

fn step_loads(
    model: &Model,
    profile: &[TargetOffset],
    workload: &'static str,
    ccds: usize,
    all: CoreSet,
) -> Result<Vec<(R7PartRole, CoreSet)>, ShapeError> {
    let mut loads = Vec::new();
    for ccd in 0..ccds {
        let full = ccd_cores(ccd, all);
        loads.push((R7PartRole::Ccd, full));
        let mut loaded: Vec<CoreId> = full.iter().collect();
        loop {
            let load = Load {
                regime: Regime::R7,
                workload,
                cores: loaded.iter().copied().collect(),
                duration: Duration::ZERO,
            };
            let Some(lanes) = model.r7_requests(profile, &load)? else {
                break;
            };
            loaded = next_partial(&loaded, &lanes);
            if loaded.len() < 2 {
                break;
            }
            loads.push((R7PartRole::Partial, loaded.iter().copied().collect()));
        }
    }
    if ccds > 1 {
        loads.push((R7PartRole::AllCore, all));
    }
    Ok(loads)
}

fn core(index: usize) -> Option<CoreId> {
    CoreId::new(i64::try_from(index).ok()?).ok()
}

/// What every row of a report is evaluated with: the configuration's short trial, the horizons and the
/// counterfactual extra short passes.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub short_trial_s: u32,
    pub launches: u32,
    pub seconds: u32,
    pub extra_short: Vec<NonZeroU32>,
}

impl Settings {
    /// `obligation` with `extra` more passes of its short trials: a counterfactual requirement.
    pub fn with_extra_short(&self, obligation: &Obligation, extra: NonZeroU32) -> Obligation {
        Obligation {
            requirements: obligation
                .requirements
                .iter()
                .map(|r| Requirement {
                    count: if r.trial.get() == self.short_trial_s {
                        r.count.saturating_add(extra.get())
                    } else {
                        r.count
                    },
                    ..*r
                })
                .collect(),
        }
    }
}

/// Writes the table of `blocks`: per part, its current requirement and each counterfactual one, and for a machine
/// step every part chained.
pub fn report(
    w: &mut impl io::Write,
    header: &[String],
    settings: &Settings,
    blocks: &[Block],
) -> io::Result<()> {
    writeln!(
        w,
        "pass-cost: counterfactual study for #490 at a fixed profile; never tuner evidence."
    )?;
    for line in header {
        writeln!(w, "pass-cost: {line}")?;
    }
    writeln!(
        w,
        "pass-cost: horizons {} launches and {} h",
        settings.launches,
        float(f64::from(settings.seconds) / 3600.0, 2)
    )?;
    let launches = format!("done≤{}", settings.launches);
    let hours = format!("done≤{}h", float(f64::from(settings.seconds) / 3600.0, 2));
    for block in blocks {
        match &block.source {
            Source::Hazard { per_hour } => {
                writeln!(w, "hazard {}/h", float(*per_hour, 2))?;
            }
            Source::Step { workload } => writeln!(w, "workload {workload}")?,
        }
        writeln!(
            w,
            "  {:<36} {:<14} {:<14} {:>7} {:>9} {:>8} {:>9} {:>10} {:>8} {:>7} {:>9} {:>7}",
            "part",
            "passes",
            "fail_p",
            "first",
            "launches",
            "hours",
            launches,
            hours,
            "spent_n",
            "spent_h",
            "+launches",
            "+hours"
        )?;
        for part in &block.parts {
            rows(w, settings, &part_label(part), None, &part.obligation)?;
        }
        if matches!(block.source, Source::Step { .. }) && block.parts.len() > 1 {
            let parts: Vec<Obligation> = block.parts.iter().map(|p| p.obligation.clone()).collect();
            if let Some(step) = Obligation::chain(&parts) {
                let summary = format!("{} parts", parts.len());
                rows(w, settings, "step", Some(&summary), &step)?;
            }
        }
    }
    writeln!(
        w,
        "pass-cost: first: probability every required trial passes on its first launch, the chance the part qualifies at this profile."
    )?;
    writeln!(
        w,
        "pass-cost: launches, hours, done and spent: COUNTERFACTUAL retries at the same profile until met, a failure resetting its own class's passes and charged its full duration; the tuner backs off instead. Censored share = 1 - done."
    )?;
    writeln!(
        w,
        "pass-cost: +N short rows: COUNTERFACTUAL requirements with N more short passes; +launches and +hours are their expected extra cost over the current requirement."
    )
}

fn part_label(part: &Part) -> String {
    let role = match part.role {
        R7PartRole::Ccd => "ccd",
        R7PartRole::Partial => "partial",
        R7PartRole::AllCore => "all-core",
    };
    match (part.role, part.cores) {
        (R7PartRole::AllCore, _) | (_, None) => role.to_owned(),
        (R7PartRole::Ccd | R7PartRole::Partial, Some(cores)) => {
            let ids: Vec<String> = cores.iter().map(|c| c.get().to_string()).collect();
            format!("{role} {}", ids.join(","))
        }
    }
}

/// The rows of `obligation` and of each of its counterfactuals; `summary` replaces the passes and failure
/// probabilities of a chained step.
fn rows(
    w: &mut impl io::Write,
    settings: &Settings,
    label: &str,
    summary: Option<&str>,
    obligation: &Obligation,
) -> io::Result<()> {
    row(w, settings, label, summary, obligation, None)?;
    for &extra in &settings.extra_short {
        let counterfactual = settings.with_extra_short(obligation, extra);
        row(
            w,
            settings,
            &format!("{label} +{extra} short"),
            summary,
            &counterfactual,
            Some(obligation),
        )?;
    }
    Ok(())
}

fn row(
    w: &mut impl io::Write,
    settings: &Settings,
    label: &str,
    summary: Option<&str>,
    obligation: &Obligation,
    current: Option<&Obligation>,
) -> io::Result<()> {
    let (passes, failures) = match summary {
        Some(summary) => (summary.to_owned(), String::new()),
        None => (
            obligation
                .requirements
                .iter()
                .map(|r| format!("{}x{}s", r.count, r.trial.get()))
                .collect::<Vec<_>>()
                .join("+"),
            obligation
                .requirements
                .iter()
                .map(|r| float(r.failure, 4))
                .collect::<Vec<_>>()
                .join("/"),
        ),
    };
    let by_launches = obligation.within_launches(settings.launches);
    let by_seconds = obligation.within_seconds(settings.seconds);
    let (more_launches, more_hours) = match current {
        None => (String::new(), String::new()),
        Some(current) if !current.expected_launches().is_finite() => {
            ("n/a".to_owned(), "n/a".to_owned())
        }
        Some(current) => (
            expected(
                obligation.expected_launches() - current.expected_launches(),
                1,
            ),
            expected(
                (obligation.expected_seconds() - current.expected_seconds()) / 3600.0,
                2,
            ),
        ),
    };
    let line = format!(
        "  {:<36} {:<14} {:<14} {:>7} {:>9} {:>8} {:>9} {:>10} {:>8} {:>7} {:>9} {:>7}",
        label,
        passes,
        failures,
        float(obligation.first_attempt(), 4),
        expected(obligation.expected_launches(), 1),
        expected(obligation.expected_seconds() / 3600.0, 2),
        float(by_launches.done, 4),
        float(by_seconds.done, 4),
        float(by_launches.spent, 1),
        float(by_seconds.spent / 3600.0, 2),
        more_launches,
        more_hours,
    );
    writeln!(w, "{}", line.trim_end())
}

fn float(value: f64, decimals: usize) -> String {
    format!("{value:.decimals$}")
}

fn expected(value: f64, decimals: usize) -> String {
    if value.is_finite() {
        float(value, decimals)
    } else {
        "never".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;
    use togi_sim::definition::SharedVoltageDef;

    use super::*;
    use crate::metrics::tests::rail;

    fn secs(seconds: u32) -> Secs {
        Secs::new(NonZeroU32::new(seconds).unwrap())
    }

    fn req(seconds: u32, count: u32, failure: f64) -> Requirement {
        Requirement {
            trial: secs(seconds),
            count,
            failure,
        }
    }

    fn obligation(requirements: &[Requirement]) -> Obligation {
        Obligation::new(requirements.to_vec()).unwrap()
    }

    /// Three short passes and one long pass at the two-CCD defaults.
    fn part(short: f64, long: f64) -> Obligation {
        obligation(&[req(120, 3, short), req(300, 1, long)])
    }

    fn assert_close(got: f64, want: f64) {
        assert!(
            (got - want).abs() <= 1e-9 * want.abs().max(1.0),
            "{got} != {want}"
        );
    }

    #[test]
    fn a_trial_that_never_fails_needs_exactly_its_passes_and_their_seconds() {
        let part = part(0.0, 0.0);
        assert_eq!(part.first_attempt(), 1.0);
        assert_eq!(part.expected_launches(), 4.0);
        assert_eq!(part.expected_seconds(), 660.0);
        assert_eq!(part.within_launches(3).done, 0.0);
        assert_eq!(
            part.within_launches(40),
            Horizon {
                done: 1.0,
                spent: 4.0
            }
        );
        assert_eq!(part.within_seconds(659).done, 0.0);
        assert_eq!(
            part.within_seconds(14_400),
            Horizon {
                done: 1.0,
                spent: 660.0
            }
        );
    }

    #[test]
    fn a_trial_that_always_fails_is_never_met_and_spends_the_whole_horizon() {
        let part = part(0.0, 1.0);
        assert_eq!(part.first_attempt(), 0.0);
        assert_eq!(part.expected_launches(), f64::INFINITY);
        assert_eq!(part.expected_seconds(), f64::INFINITY);
        assert_eq!(
            part.within_launches(10),
            Horizon {
                done: 0.0,
                spent: 10.0
            }
        );
        assert_eq!(
            part.within_seconds(1000),
            Horizon {
                done: 0.0,
                spent: 1000.0
            }
        );
    }

    #[test_case(0.1 ; "rare")]
    #[test_case(0.5 ; "even")]
    #[test_case(0.9 ; "frequent")]
    fn one_pass_waits_a_geometric_number_of_launches(p: f64) {
        let single = obligation(&[req(60, 1, p)]);
        assert_close(single.expected_launches(), 1.0 / (1.0 - p));
        let horizon = single.within_launches(7);
        assert_close(horizon.done, 1.0 - p.powi(7));
        assert_close(horizon.spent, (1.0 - p.powi(7)) / (1.0 - p));
    }

    #[test]
    fn five_passes_in_a_row_at_even_odds_wait_sixty_two_launches() {
        let streak = obligation(&[req(120, 5, 0.5)]);
        assert_eq!(streak.first_attempt(), 0.03125);
        assert_eq!(streak.expected_launches(), 62.0);
        assert_eq!(streak.within_launches(5).done, 0.03125);
        let long_run = streak.within_launches(5000);
        assert_close(long_run.done, 1.0);
        assert_close(long_run.spent, 62.0);
    }

    #[test]
    fn mixed_durations_multiply_their_pass_probabilities_and_add_their_waits() {
        let part = part(0.1, 0.2);
        assert_close(part.first_attempt(), 0.9_f64.powi(3) * 0.8);
        let short_launches = 1.0 / 0.9 + 1.0 / 0.81 + 1.0 / 0.729;
        assert_close(part.expected_launches(), short_launches + 1.0 / 0.8);
        assert_close(
            part.expected_seconds(),
            120.0 * short_launches + 300.0 / 0.8,
        );
    }

    #[test]
    fn the_dynamic_programs_converge_to_the_closed_form_expectations() {
        let part = part(0.2, 0.4);
        let launches = part.within_launches(2000);
        assert_close(launches.done, 1.0);
        assert_close(launches.spent, part.expected_launches());
        let seconds = part.within_seconds(400_000);
        assert_close(seconds.done, 1.0);
        assert_close(seconds.spent, part.expected_seconds());
    }

    /// 3×120 s + 1×300 s: 660 s without a failure. A short failure restarts the three in a row: failing the first
    /// short trial once adds 120 s; failing it twice, or failing the second, adds 240 s; a long failure adds 300 s.
    #[test]
    fn a_censored_horizon_counts_only_the_outcomes_that_end_within_it() {
        let (p, l) = (0.1, 0.2);
        let (q, m) = (1.0 - p, 1.0 - l);
        let part = part(p, l);
        let clean = q.powi(3) * m;
        assert_close(part.within_seconds(659).done, 0.0);
        assert_close(part.within_seconds(660).done, clean);
        assert_close(part.within_seconds(779).done, clean);
        assert_close(part.within_seconds(780).done, clean * (1.0 + p));
        assert_close(
            part.within_seconds(900).done,
            clean * (1.0 + p + p * p + q * p),
        );
        assert_close(
            part.within_seconds(960).done,
            clean * (1.0 + p + p * p + q * p + l),
        );
    }

    #[test]
    fn a_failure_resets_only_its_own_class() {
        let part = obligation(&[req(120, 1, 0.0), req(300, 1, 0.5)]);
        assert_eq!(part.within_launches(2).done, 0.5);
        assert_eq!(part.within_launches(3).done, 0.75);
        assert_eq!(part.expected_launches(), 3.0);
    }

    #[test]
    fn one_second_trials_give_the_same_horizon_in_seconds_as_in_launches() {
        let part = obligation(&[req(1, 3, 0.3), req(1, 1, 0.6)]);
        let by_launches = part.within_launches(12);
        let by_seconds = part.within_seconds(12);
        assert_close(by_seconds.done, by_launches.done);
        assert_close(by_seconds.spent, by_launches.spent);
    }

    #[test]
    fn an_obligation_refuses_what_it_cannot_evaluate() {
        assert_eq!(Obligation::new(vec![]), Err(ObligationError::Empty));
        assert_eq!(
            Obligation::new(vec![req(120, 0, 0.5)]),
            Err(ObligationError::NoPasses)
        );
        assert_eq!(
            Obligation::new(vec![req(120, 3, 1.5)]),
            Err(ObligationError::Probability(1.5))
        );
        assert!(matches!(
            Obligation::new(vec![req(120, 3, f64::NAN)]),
            Err(ObligationError::Probability(_))
        ));
    }

    #[test_case(0.0, 120, 0.0 ; "no hazard")]
    #[test_case(3600.0, 1, 1.0 - (-1.0_f64).exp() ; "one expected failure")]
    #[test_case(1.0, 1800, 1.0 - (-0.5_f64).exp() ; "half an expected failure")]
    fn a_constant_hazard_fails_a_trial_with_its_poisson_probability(
        per_hour: f64,
        seconds: u32,
        want: f64,
    ) {
        assert_close(constant_failure(per_hour, secs(seconds)), want);
    }

    #[test]
    fn constant_hazards_on_two_ccds_give_the_ccd_and_all_core_requirements() {
        let blocks = hazard_blocks(&Config::default(), 2, &[2.0]).unwrap();
        let [block] = blocks.as_slice() else {
            panic!("one hazard gives one block: {blocks:?}");
        };
        assert_eq!(block.source, Source::Hazard { per_hour: 2.0 });
        let short = constant_failure(2.0, secs(120));
        assert_eq!(
            block.parts,
            [
                Part {
                    role: R7PartRole::Ccd,
                    cores: None,
                    obligation: obligation(&[
                        req(120, 3, short),
                        req(300, 1, constant_failure(2.0, secs(300)))
                    ]),
                },
                Part {
                    role: R7PartRole::AllCore,
                    cores: None,
                    obligation: obligation(&[
                        req(120, 3, short),
                        req(600, 1, constant_failure(2.0, secs(600)))
                    ]),
                },
            ]
        );
    }

    #[test]
    fn a_constant_hazard_on_one_ccd_has_one_part_with_the_whole_long_trial() {
        let blocks = hazard_blocks(&Config::default(), 1, &[0.0]).unwrap();
        assert_eq!(blocks[0].parts.len(), 1);
        assert_eq!(
            blocks[0].parts[0].obligation,
            obligation(&[req(120, 3, 0.0), req(1200, 1, 0.0)])
        );
    }

    #[test]
    fn extra_short_passes_add_to_the_short_requirement_only() {
        let settings = Settings {
            short_trial_s: 120,
            launches: 40,
            seconds: 14_400,
            extra_short: vec![],
        };
        let current = part(0.1, 0.2);
        assert_eq!(
            settings.with_extra_short(&current, NonZeroU32::new(2).unwrap()),
            obligation(&[req(120, 5, 0.1), req(300, 1, 0.2)])
        );
    }

    /// A shared-rail machine whose only failures are its background, `per_second`: the rail's own rate is
    /// negligible.
    fn background_only(per_second: f64) -> Model {
        let mut definition = rail::definition();
        let rail: &mut SharedVoltageDef = definition.shared_voltage.as_mut().unwrap();
        rail.rate = 1e-300;
        rail.background_rate = per_second;
        Model::new(&definition, 1).unwrap()
    }

    fn ids(cores: Option<CoreSet>) -> Vec<u8> {
        cores.unwrap().iter().map(CoreId::get).collect()
    }

    #[test]
    fn a_machine_step_fails_each_trial_by_its_background_over_the_trial() {
        let b = 2.0 / 3600.0;
        let model = background_only(b);
        let profile = rail::profile();
        let workload = workloads(Regime::R7)[0].id;
        let query = MachineQuery {
            profile: &profile,
            workload: Some(workload),
            cores: None,
        };
        let blocks = machine_blocks(&Config::default(), &model, &query).unwrap();
        let [block] = blocks.as_slice() else {
            panic!("one workload gives one block: {blocks:?}");
        };
        let roles: Vec<(R7PartRole, Vec<u8>)> = block
            .parts
            .iter()
            .map(|part| (part.role, ids(part.cores)))
            .collect();
        assert_eq!(
            roles,
            [
                (R7PartRole::Ccd, (0..8).collect()),
                (R7PartRole::Ccd, (8..16).collect()),
                (R7PartRole::AllCore, (0..16).collect()),
            ]
        );
        for part in &block.parts {
            for r in part.obligation.requirements() {
                assert_close(r.failure, -(-b * f64::from(r.trial.get())).exp_m1());
            }
        }
        let parts: Vec<Obligation> = block.parts.iter().map(|p| p.obligation.clone()).collect();
        let step = Obligation::chain(&parts).unwrap();
        assert_close(step.first_attempt(), (-b * 2280.0).exp());
    }

    #[test]
    fn a_machine_step_includes_the_request_ordered_partials() {
        let mut definition = rail::definition();
        rail::each_workload(&mut definition, |_, w| w.core[0].base_v = 1.272);
        let model = Model::new(&definition, 1).unwrap();
        let profile = rail::profile();
        let query = MachineQuery {
            profile: &profile,
            workload: Some(workloads(Regime::R7)[0].id),
            cores: None,
        };
        let blocks = machine_blocks(&Config::default(), &model, &query).unwrap();
        let roles: Vec<(R7PartRole, Vec<u8>)> = blocks[0]
            .parts
            .iter()
            .map(|part| (part.role, ids(part.cores)))
            .collect();
        assert_eq!(
            roles,
            [
                (R7PartRole::Ccd, (0..8).collect()),
                (R7PartRole::Partial, (1..8).collect()),
                (R7PartRole::Ccd, (8..16).collect()),
                (R7PartRole::AllCore, (0..16).collect()),
            ]
        );
    }

    #[test]
    fn a_machine_part_by_its_cores_is_one_partial_and_needs_two_cores() {
        let model = background_only(1.0 / 3600.0);
        let profile = rail::profile();
        let cores =
            |ids: &[i64]| -> CoreSet { ids.iter().map(|&id| CoreId::new(id).unwrap()).collect() };
        let pair = MachineQuery {
            profile: &profile,
            workload: Some(workloads(Regime::R7)[0].id),
            cores: Some(cores(&[14, 15])),
        };
        let blocks = machine_blocks(&Config::default(), &model, &pair).unwrap();
        assert_eq!(blocks[0].parts.len(), 1);
        assert_eq!(blocks[0].parts[0].role, R7PartRole::Partial);
        let single = MachineQuery {
            cores: Some(cores(&[14])),
            ..pair.clone()
        };
        assert!(matches!(
            machine_blocks(&Config::default(), &model, &single),
            Err(MachineError::SingleCore)
        ));
        let unknown = MachineQuery {
            workload: Some("nonesuch"),
            ..pair
        };
        assert!(matches!(
            machine_blocks(&Config::default(), &model, &unknown),
            Err(MachineError::Workload(id)) if id == "nonesuch"
        ));
    }

    #[test]
    fn the_report_labels_every_retry_and_extra_pass_counterfactual() {
        let settings = Settings {
            short_trial_s: 120,
            launches: 40,
            seconds: 14_400,
            extra_short: vec![NonZeroU32::new(2).unwrap()],
        };
        let header = [
            "short_trial_s 120, checking_all_core_s 1200".to_owned(),
            "constant hazards on 2 CCDs; a partial's requirement is its CCD part's".to_owned(),
        ];
        let blocks = hazard_blocks(&Config::default(), 2, &[2.0]).unwrap();
        let mut report = Vec::new();
        super::report(&mut report, &header, &settings, &blocks).unwrap();
        let expected = include_str!(
            "snapshots/togi_tool_bench__pass_cost__tests__the_report_labels_every_retry_and_extra_pass_counterfactual.snap"
        );
        assert_eq!(
            String::from_utf8(report).unwrap(),
            expected
                .splitn(3, "---")
                .nth(2)
                .unwrap()
                .trim_start_matches('\n')
        );
    }
}
