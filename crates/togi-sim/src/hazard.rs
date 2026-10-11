//! The pure hazard model.
//!
//! Every way a trial can fail is a [`HazardSource`] with a constant rate and an optional start. One
//! enumeration, [`Model::visit_sources`], produces them in a fixed order, and everything that needs
//! them walks it: the steady [`Model::hazard`], the integrated [`Model::failure_probability`], the
//! [`Model::r7_failure_share`] and, in the runtime, the competing failure-time draws. A failure time
//! drawn from a source with [`Onset::failure_time`] is the inverse of the exposure
//! [`failure_probability`](Model::failure_probability) integrates, so scores and draws agree.

use std::time::Duration;
use togi_machine::{CoreId, CoreSet, Regime, TargetOffset};

use crate::cores::ids;
use crate::error::ShapeError;
use crate::model::{Joint, Limits, Model, Onset};
use crate::random::Domain;
use crate::vocab::regime_index;
use crate::voltage::{Voltage, voltage_state};

/// What a trial loads: its regime and workload, the cores running it and its intended duration.
#[derive(Clone, Copy, Debug)]
pub struct Load<'a> {
    pub regime: Regime,
    pub workload: &'a str,
    pub cores: CoreSet,
    pub duration: Duration,
}

/// Which part of the machine fails. A trial that loads no core has only idle and background sources.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceKind {
    /// A loaded core at its limit and flat rate.
    Core(CoreId),
    /// An unloaded core at its idle limit and flat rate.
    Idle(CoreId),
    /// A loaded core below its required voltage on the shared rail.
    Voltage(CoreId),
    /// The residual hazard of a loaded CCD.
    Ccd(u8),
    /// The joint at this index of [`Model::joints`].
    Joint(usize),
    /// The platform's failure rate on a multi-core shared-voltage R7 trial.
    Background,
}

/// A failure source: failures per second from `after_s` seconds into the trial, multiplied by
/// `1 + boost` during the model's onset window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HazardSource {
    pub kind: SourceKind,
    pub rate: f64,
    pub after_s: f64,
}

impl HazardSource {
    /// The random domain of this source's failure draw.
    pub fn domain(&self) -> Domain {
        match self.kind {
            SourceKind::Core(_) => Domain::Core,
            SourceKind::Idle(_) => Domain::Idle,
            SourceKind::Voltage(_) => Domain::Voltage,
            SourceKind::Ccd(ccd) => Domain::Ccd(ccd),
            SourceKind::Joint(index) => Domain::Joint(u32::try_from(index).unwrap_or(u32::MAX)),
            SourceKind::Background => Domain::Background,
        }
    }
}

impl Onset {
    /// Seconds of exposure to a source that starts at `after_s` within a trial of `duration_s`, counting
    /// the boosted seconds `1 + boost` times: the [`failures`](Self::failures) of a source of rate 1.
    pub fn exposure(self, after_s: f64, duration_s: f64) -> f64 {
        self.failures(after_s, 1.0, duration_s)
    }

    /// The expected failures of a source of `rate` that starts at `after_s` within a trial of
    /// `duration_s`: the boosted rate `rate · (1 + boost)` times the seconds inside the onset window, plus
    /// `rate` times the seconds after it. Each phase is evaluated without an intermediate that overflows or
    /// underflows, so the count is infinite only when the true count exceeds `f64::MAX`, and a phase with
    /// no seconds adds nothing.
    pub fn failures(self, after_s: f64, rate: f64, duration_s: f64) -> f64 {
        let seconds = duration_s - after_s;
        if seconds <= 0.0 {
            return 0.0;
        }
        let early = self.window_s - after_s;
        if early > 0.0 && self.boost > 0.0 {
            if seconds <= early {
                return self.boosted(rate, seconds);
            }
            return self.boosted(rate, early) + rate * (seconds - early);
        }
        rate * seconds
    }

    /// The time, in seconds into the trial, at which a source of `rate` that starts at `after_s` has
    /// accumulated `cumulative` expected failures: the inverse of [`failures`](Self::failures). Drawing
    /// `cumulative` from an exponential distribution of mean 1 samples the source's failure time. The draw
    /// lands inside the onset window exactly when `cumulative` is at most the window's count of failures.
    pub fn failure_time(self, after_s: f64, rate: f64, cumulative: f64) -> f64 {
        let early = self.window_s - after_s;
        if early > 0.0 && self.boost > 0.0 {
            let window = self.boosted(rate, early);
            if cumulative <= window {
                return after_s + quotient(cumulative, rate, 1.0 + self.boost).min(early);
            }
            return after_s + early + (cumulative - window) / rate;
        }
        after_s + cumulative / rate
    }

    /// Failures of a source of `rate` over `seconds` inside the onset window.
    fn boosted(self, rate: f64, seconds: f64) -> f64 {
        product(rate, 1.0 + self.boost, seconds)
    }
}

/// `a · b · c` for nonnegative factors, in the log domain when a partial product would overflow or
/// underflow, so the result is infinite or zero only when the true product is.
fn product(a: f64, b: f64, c: f64) -> f64 {
    let partial = a * b;
    let direct = partial * c;
    if partial.is_normal() && direct.is_normal() {
        return direct;
    }
    if a == 0.0 || b == 0.0 || c == 0.0 {
        return 0.0;
    }
    (a.ln() + b.ln() + c.ln()).exp()
}

/// `c / (a · b)` for positive `a` and `b` and nonnegative `c`, by the same rule as [`product`].
fn quotient(c: f64, a: f64, b: f64) -> f64 {
    let divisor = a * b;
    let direct = c / divisor;
    if divisor.is_normal() && (direct.is_normal() || c == 0.0) {
        return direct;
    }
    if c == 0.0 {
        return 0.0;
    }
    (c.ln() - a.ln() - b.ln()).exp()
}

/// The most counts past a limit an offset can reach: a limit of 1 at offset -50 is 51 counts past it,
/// 50 growth steps beyond the first.
const DEEPEST_STEPS: f64 = 50.0;

/// `rate · growth^steps`, the failures per second `steps + 1` counts past a limit. When the power alone
/// would underflow or overflow, the product is taken in the log domain, so a representable rate is never
/// lost on the way; a zero rate stays zero.
pub(crate) fn grown(rate: f64, growth: f64, steps: f64) -> f64 {
    if rate == 0.0 {
        return 0.0;
    }
    let factor = growth.powf(steps);
    if factor.is_normal() {
        rate * factor
    } else {
        steps.mul_add(growth.ln(), rate.ln()).exp()
    }
}

/// The largest rate a limit gives a core anywhere in the range of limits (-50..=1) and offsets
/// (-50..=0): the near-limit rate, or the past-limit rate at either end of its growth.
pub(crate) fn largest_limit_rate(past_limit_rate: f64, growth: f64, near_limit_rate: f64) -> f64 {
    near_limit_rate
        .max(past_limit_rate)
        .max(grown(past_limit_rate, growth, DEEPEST_STEPS))
}

/// Counts and ratios of small integers as `f64`.
pub(crate) fn exact(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

pub(crate) fn steps(value: i64) -> f64 {
    f64::from(i32::try_from(value).unwrap_or(i32::MAX))
}

impl Model {
    pub(crate) fn check_shape(
        &self,
        profile: &[TargetOffset],
        load: &Load<'_>,
    ) -> Result<(), ShapeError> {
        if profile.len() != self.cores() {
            return Err(ShapeError::Profile {
                expected: self.cores(),
                found: profile.len(),
            });
        }
        match load.cores.last() {
            Some(core) if core.index() >= self.cores() => Err(ShapeError::LoadedCore {
                core: core.get(),
                cores: self.cores(),
            }),
            _ => Ok(()),
        }
    }

    /// The shared-voltage model when `load` is a multi-core R7 trial on a machine that has one.
    pub(crate) fn shared_voltage(&self, load: &Load<'_>) -> Option<&Voltage> {
        (load.cores.len() > 1 && load.regime == Regime::R7)
            .then(|| self.voltage())
            .flatten()
    }

    /// Calls `visit` once for every source of failure of a trial with `profile` applied, in the fixed
    /// order ties are broken in: cores by ID (loaded or idle), shared-rail cores by ID, CCD 0 then 1,
    /// joints in file order, then the background. Sources with no rate are skipped.
    ///
    /// A multi-core R7 load on a shared-voltage machine replaces its loaded cores', CCDs' and joints'
    /// sources with the rail's.
    pub fn visit_sources(
        &self,
        profile: &[TargetOffset],
        load: &Load<'_>,
        mut visit: impl FnMut(HazardSource),
    ) -> Result<(), ShapeError> {
        self.check_shape(profile, load)?;
        let regime = load.regime;
        let shared = self
            .shared_voltage(load)
            .map(|voltage| (voltage, voltage_state(voltage, profile, load)));
        let nonzero = profile.iter().filter(|offset| offset.get() != 0).count();
        for core in ids(self.cores()) {
            let loaded = load.cores.contains(core);
            let rate = match (loaded, shared.is_some()) {
                (true, true) => 0.0,
                (true, false) => self.loaded_rate(profile, regime, load.workload, core, nonzero),
                (false, _) => self.idle_rate(profile, core),
            };
            if rate > 0.0 {
                let kind = if loaded {
                    SourceKind::Core(core)
                } else {
                    SourceKind::Idle(core)
                };
                visit(HazardSource {
                    kind,
                    rate,
                    after_s: 0.0,
                });
            }
        }
        if let Some((voltage, state)) = &shared {
            for core in load.cores {
                let rate = state.rates[core.index()];
                if rate > 0.0 {
                    visit(HazardSource {
                        kind: SourceKind::Voltage(core),
                        rate,
                        after_s: 0.0,
                    });
                }
            }
            if voltage.background_rate > 0.0 {
                visit(HazardSource {
                    kind: SourceKind::Background,
                    rate: voltage.background_rate,
                    after_s: 0.0,
                });
            }
            return Ok(());
        }
        for ccd in 0..2u8 {
            let rate = self.ccd_rate(profile, load, regime, usize::from(ccd));
            if rate > 0.0 {
                visit(HazardSource {
                    kind: SourceKind::Ccd(ccd),
                    rate,
                    after_s: 0.0,
                });
            }
        }
        for (index, joint) in self.joints().iter().enumerate() {
            let rate = joint_rate(joint, profile, regime);
            if rate > 0.0 {
                visit(HazardSource {
                    kind: SourceKind::Joint(index),
                    rate,
                    after_s: joint.after_s,
                });
            }
        }
        Ok(())
    }

    /// The sources of [`visit_sources`](Self::visit_sources) collected.
    pub fn sources(
        &self,
        profile: &[TargetOffset],
        load: &Load<'_>,
    ) -> Result<Vec<HazardSource>, ShapeError> {
        let mut sources = Vec::new();
        self.visit_sources(profile, load, |source| sources.push(source))?;
        Ok(sources)
    }

    /// Failures per second at `profile`: the steady state, without the onset boost or any delay.
    pub fn hazard(&self, profile: &[TargetOffset], load: &Load<'_>) -> Result<f64, ShapeError> {
        let mut rate = 0.0;
        self.visit_sources(profile, load, |source| rate += source.rate)?;
        Ok(rate)
    }

    /// The probability that a trial of `load.duration` fails: every source's
    /// [`failures`](Onset::failures) over the trial, with the onset boost and delays, as
    /// `-expm1(-Σ failures)`.
    pub fn failure_probability(
        &self,
        profile: &[TargetOffset],
        load: &Load<'_>,
    ) -> Result<f64, ShapeError> {
        let duration = load.duration.as_secs_f64();
        let onset = self.params().onset;
        let mut failures = 0.0;
        self.visit_sources(profile, load, |source| {
            failures += onset.failures(source.after_s, source.rate, duration);
        })?;
        Ok(-(-failures).exp_m1())
    }

    /// A loaded core is alone when no other core has a nonzero offset.
    fn loaded_rate(
        &self,
        profile: &[TargetOffset],
        regime: Regime,
        workload: &str,
        core: CoreId,
        nonzero: usize,
    ) -> f64 {
        let limits = &self.limits()[core.index()];
        let offset = profile[core.index()].get();
        let alone = nonzero == 0 || (nonzero == 1 && offset != 0);
        let limit = limit(limits, regime, workload, alone);
        let params = self.params();
        let past = limit - offset;
        let flat = if offset == 0 { 0.0 } else { limits.flat };
        flat + if past >= 1 {
            grown(params.past_limit_rate, params.growth, steps(past - 1))
        } else {
            params.near_limit_rate
        }
    }

    fn idle_rate(&self, profile: &[TargetOffset], core: CoreId) -> f64 {
        let limits = &self.limits()[core.index()];
        if limits.idle.is_none() && limits.flat <= 0.0 {
            return 0.0;
        }
        let offset = profile[core.index()].get();
        let mut rate = if offset == 0 { 0.0 } else { limits.flat };
        if let Some(idle) = limits.idle {
            let past = idle - offset;
            if past >= 1 {
                rate += grown(
                    self.params().past_limit_rate,
                    self.params().growth,
                    steps(past - 1),
                );
            }
        }
        rate
    }

    /// The residual hazard of a loaded CCD in R7 from its mean applied depth, which any active joint with
    /// a member on that CCD replaces, even before the joint's delay has passed.
    fn ccd_rate(
        &self,
        profile: &[TargetOffset],
        load: &Load<'_>,
        regime: Regime,
        ccd: usize,
    ) -> f64 {
        let Some(parameters) = self.ccd() else {
            return 0.0;
        };
        if regime != Regime::R7 {
            return 0.0;
        }
        let size = self.cores() / 2;
        let on_ccd = |core: CoreId| core.index() / size == ccd;
        if !load.cores.iter().any(on_ccd) {
            return 0.0;
        }
        let explained = self.joints().iter().any(|joint| {
            joint_rate(joint, profile, regime) > 0.0
                && joint.members.iter().any(|member| on_ccd(member.core))
        });
        if explained {
            return 0.0;
        }
        let depth: i64 = profile[ccd * size..(ccd + 1) * size]
            .iter()
            .map(|offset| -offset.get())
            .sum();
        let mean = steps(depth) / exact(size);
        (parameters.log_rate + parameters.effect[ccd] + parameters.slope * (mean - 25.0)).exp()
    }
}

fn limit(limits: &Limits, regime: Regime, workload: &str, alone: bool) -> i64 {
    if let Some((_, limit)) = limits.workload.iter().find(|(id, _)| id == workload) {
        return *limit;
    }
    let index = regime_index(regime);
    if alone && index < limits.alone.len() {
        limits.alone[index]
    } else {
        limits.together[index]
    }
}

fn joint_rate(joint: &Joint, profile: &[TargetOffset], regime: Regime) -> f64 {
    if !joint.regimes.covers(regime)
        || joint
            .members
            .iter()
            .any(|member| profile[member.core.index()] > member.offset)
    {
        return 0.0;
    }
    joint.rate
}
