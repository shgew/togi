//! The pure hazard model.
//!
//! Every way a trial can fail is a [`HazardSource`] with a constant rate and an optional start. One
//! enumeration, [`Model::visit_sources`], produces them in a fixed order, and everything that needs
//! them walks it: the steady [`Model::hazard`], the integrated [`Model::failure_probability`], the
//! [`Model::r7_failure_share`] and, in the runtime, the competing failure-time draws. A failure time
//! drawn from a source with [`Onset::failure_time`] is the inverse of the exposure
//! [`failure_probability`](Model::failure_probability) integrates, so scores and draws agree.

use std::time::Duration;
use togi_machine::{CoreId, Regime, TargetOffset};

use crate::coreset::{CoreSet, ids};
use crate::error::ShapeError;
use crate::model::{Joint, Limits, Model, Onset};
use crate::random::Domain;
use crate::vocab::RegimeId;
use crate::voltage::{Voltage, voltage_state};

/// What a trial loads: its regime and workload, the cores running it and its intended duration.
#[derive(Clone, Copy, Debug)]
pub struct Load<'a> {
    pub regime: &'a Regime,
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
    /// the boosted seconds `1 + boost` times.
    pub fn exposure(self, after_s: f64, duration_s: f64) -> f64 {
        (duration_s - after_s).max(0.0)
            + self.boost * (duration_s.min(self.window_s) - after_s).max(0.0)
    }

    /// The time, in seconds into the trial, at which a source of `rate` that starts at `after_s` has
    /// accumulated `cumulative` expected failures: the inverse of [`exposure`](Self::exposure). Drawing
    /// `cumulative` from an exponential distribution of mean 1 samples the source's failure time.
    pub fn failure_time(self, after_s: f64, rate: f64, cumulative: f64) -> f64 {
        let early = self.window_s - after_s;
        if early > 0.0 && self.boost > 0.0 {
            let boosted = 1.0 + self.boost;
            let seconds = cumulative / (rate * boosted);
            if seconds <= early {
                return after_s + seconds;
            }
            return after_s + early + (cumulative - early * rate * boosted) / rate;
        }
        after_s + cumulative / rate
    }
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
        match load.cores.highest() {
            Some(core) if core.index() >= self.cores() => Err(ShapeError::LoadedCore {
                core: core.get(),
                cores: self.cores(),
            }),
            _ => Ok(()),
        }
    }

    /// The shared-voltage model when `load` is a multi-core R7 trial on a machine that has one.
    pub(crate) fn shared_voltage(&self, load: &Load<'_>) -> Option<&Voltage> {
        (load.cores.len() > 1 && RegimeId::of(load.regime) == Some(RegimeId::R7))
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
        let regime = RegimeId::of(load.regime);
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
            for core in load.cores.iter() {
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

    /// The probability that a trial of `load.duration` fails: every source integrated over its exposure,
    /// with the onset boost and delays, as `-expm1(-Σ rate · exposure)`.
    pub fn failure_probability(
        &self,
        profile: &[TargetOffset],
        load: &Load<'_>,
    ) -> Result<f64, ShapeError> {
        let duration = load.duration.as_secs_f64();
        let onset = self.params().onset;
        let mut hazard = 0.0;
        self.visit_sources(profile, load, |source| {
            hazard += source.rate * onset.exposure(source.after_s, duration);
        })?;
        Ok(-(-hazard).exp_m1())
    }

    /// A loaded core is alone when no other core has a nonzero offset.
    fn loaded_rate(
        &self,
        profile: &[TargetOffset],
        regime: Option<RegimeId>,
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
            params.past_limit_rate * params.growth.powf(steps(past - 1))
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
                rate += self.params().past_limit_rate * self.params().growth.powf(steps(past - 1));
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
        regime: Option<RegimeId>,
        ccd: usize,
    ) -> f64 {
        let Some(parameters) = self.ccd() else {
            return 0.0;
        };
        if regime != Some(RegimeId::R7) {
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

fn limit(limits: &Limits, regime: Option<RegimeId>, workload: &str, alone: bool) -> i64 {
    if let Some((_, limit)) = limits.workload.iter().find(|(id, _)| id == workload) {
        return *limit;
    }
    match regime {
        None => 0,
        Some(regime) if alone && regime.index() < limits.alone.len() => {
            limits.alone[regime.index()]
        }
        Some(regime) => limits.together[regime.index()],
    }
}

fn joint_rate(joint: &Joint, profile: &[TargetOffset], regime: Option<RegimeId>) -> f64 {
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
