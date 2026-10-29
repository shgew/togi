//! Per-core thermal history, onset clocks, integrated hazards and draws for successor truth.
//!
//! Every competing source (one per loaded physical core, one per sparse joint pair, platform
//! background) comes from the same enumeration, which drives integration, observed-slice
//! accounting and draws. A caller advances history only by the exposure actually completed.

use std::time::Duration;

use rand::RngExt;
use togi_machine::{CcdId, CoreId, CoreInfo, PerCore};

use super::super::{
    Family, HOUR, THERMAL_TAU_S, THERMAL_THRESHOLD_S, TrialClass, TruthError, class_key,
};
use super::{Fixture, Profile, Scenario};
use crate::random::{self, Domain};

/// Distinguishes successor draws from the frozen day/night streams.
const STREAM_SALT: u64 = 0x5355_4343_4553_534f;
/// Uninterrupted exposure of one exact class before a delayed core hazard begins.
const ONSET_DELAY: Duration = Duration::from_mins(10);

/// An uninterrupted run of one exact class, and of one exact profile once a profile is known.
#[derive(Clone, Debug)]
struct Run {
    class: TrialClass,
    profile: Option<Profile>,
    elapsed: Duration,
}

impl Run {
    /// A profile-free advance continues a run of any profile; a profile continues only the
    /// same profile or a run that has not yet seen one.
    fn continues(&self, class: &TrialClass, profile: Option<&Profile>) -> bool {
        self.class == *class
            && match (&self.profile, profile) {
                (Some(own), Some(next)) => own == next,
                (Some(_) | None, None) | (None, Some(_)) => true,
            }
    }
}

/// Explicit thermal memory per CCD and the uninterrupted exact-class clock.
///
/// Sleep and [`TruthState::interrupt`] end the clock; sleep also cools, interruption keeps heat.
#[derive(Clone, Debug)]
pub struct TruthState {
    topology: Vec<CoreInfo>,
    heat_s: Vec<(CcdId, f64)>,
    run: Option<Run>,
}

impl TruthState {
    pub fn cold(topology: &[CoreInfo]) -> Result<Self, TruthError> {
        if topology.is_empty() {
            return Err(TruthError::Invalid("history topology"));
        }
        let mut cores = topology.iter().map(|info| info.core).collect::<Vec<_>>();
        cores.sort_unstable();
        if cores.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(TruthError::Invalid("duplicate core"));
        }
        let mut ccds = topology.iter().map(|info| info.ccd).collect::<Vec<_>>();
        ccds.sort_unstable();
        ccds.dedup();
        Ok(Self {
            topology: topology.to_vec(),
            heat_s: ccds.into_iter().map(|ccd| (ccd, 0.0)).collect(),
            run: None,
        })
    }

    /// Advance by actual elapsed time, not intended trial duration; `None` is sleep.
    ///
    /// The clock continues for the same exact class whatever profile ran, so a caller that
    /// does not know a profile (a reference calendar) still integrates onset.
    pub fn advance(
        &mut self,
        class: Option<&TrialClass>,
        duration: Duration,
    ) -> Result<(), TruthError> {
        self.step(class, None, duration)
    }

    /// Advance with the full profile in force. The clock restarts when the exact class or
    /// exact full profile differs from the run in progress; `None` is sleep.
    pub fn advance_profile(
        &mut self,
        class: Option<&TrialClass>,
        profile: &Profile,
        duration: Duration,
    ) -> Result<(), TruthError> {
        self.check_profile(profile)?;
        self.step(class, Some(profile), duration)
    }

    /// End the uninterrupted clock even at zero elapsed time, keeping thermal heat. Used when
    /// a failure or any other real interruption restarts the next run.
    pub fn interrupt(&mut self) {
        self.run = None;
    }

    pub fn heat_seconds(&self, ccd: CcdId) -> Option<f64> {
        self.heat_s
            .iter()
            .find_map(|(id, heat)| (*id == ccd).then_some(*heat))
    }

    fn step(
        &mut self,
        class: Option<&TrialClass>,
        profile: Option<&Profile>,
        duration: Duration,
    ) -> Result<(), TruthError> {
        if let Some(class) = class {
            self.check_class(class)?;
        }
        // Everything fallible precedes the first state change.
        let elapsed = match (class, &self.run) {
            (Some(class), Some(run)) if run.continues(class, profile) => Some(
                run.elapsed
                    .checked_add(duration)
                    .ok_or(TruthError::Invalid("uninterrupted exposure"))?,
            ),
            (Some(_) | None, Some(_) | None) => None,
        };
        let decay = (-duration.as_secs_f64() / THERMAL_TAU_S).exp();
        for (ccd, heat) in &mut self.heat_s {
            let target = if class.is_some_and(|class| loads_ccd(&self.topology, class, *ccd)) {
                THERMAL_TAU_S
            } else {
                0.0
            };
            *heat = target + (*heat - target) * decay;
        }
        self.run = match (class, elapsed) {
            (Some(_), Some(elapsed)) => self.run.take().map(|run| Run {
                profile: profile.cloned().or(run.profile),
                elapsed,
                ..run
            }),
            (Some(class), None) => Some(Run {
                class: class.clone(),
                profile: profile.cloned(),
                elapsed: duration,
            }),
            (None, _) => None,
        };
        Ok(())
    }

    fn check_class(&self, class: &TrialClass) -> Result<(), TruthError> {
        if class
            .cores
            .iter()
            .all(|core| self.topology.iter().any(|info| info.core == *core))
        {
            Ok(())
        } else {
            Err(TruthError::Invalid("class topology"))
        }
    }

    fn check_profile(&self, profile: &Profile) -> Result<(), TruthError> {
        if profile.depths().len() == self.topology.len()
            && self
                .topology
                .iter()
                .all(|info| profile.depths().get(info.core).is_some())
        {
            Ok(())
        } else {
            Err(TruthError::Invalid("profile topology"))
        }
    }

    /// Uninterrupted time of exactly this class (and profile when given).
    fn elapsed(&self, class: &TrialClass, profile: Option<&Profile>) -> Duration {
        self.run
            .as_ref()
            .filter(|run| run.continues(class, profile))
            .map_or(Duration::ZERO, |run| run.elapsed)
    }

    fn heat_of(&self, core: CoreId) -> Result<f64, TruthError> {
        let ccd = self
            .topology
            .iter()
            .find(|info| info.core == core)
            .ok_or(TruthError::Invalid("core topology"))?
            .ccd;
        self.heat_seconds(ccd)
            .ok_or(TruthError::Invalid("history topology"))
    }
}

fn loads_ccd(topology: &[CoreInfo], class: &TrialClass, ccd: CcdId) -> bool {
    class.cores.iter().any(|core| {
        topology
            .iter()
            .any(|info| info.core == *core && info.ccd == ccd)
    })
}

/// The exact thermal fixed point at the start of a repeating cycle, with no clock running.
///
/// One cold cycle yields `b` in `h_next = a * h_start + b`; the periodic heat solves it.
pub(super) fn periodic(
    topology: &[CoreInfo],
    cycle: &[(Option<&TrialClass>, Duration)],
) -> Result<TruthState, TruthError> {
    let mut state = TruthState::cold(topology)?;
    let mut total = Duration::ZERO;
    for &(class, duration) in cycle {
        total = total
            .checked_add(duration)
            .ok_or(TruthError::Invalid("periodic cycle"))?;
        if !duration.is_zero() {
            state.advance(class, duration)?;
        }
    }
    if total.is_zero() {
        return Err(TruthError::Invalid("periodic cycle"));
    }
    let recurrence = (-total.as_secs_f64() / THERMAL_TAU_S).exp();
    for (_, heat) in &mut state.heat_s {
        *heat /= 1.0 - recurrence;
    }
    state.run = None;
    Ok(state)
}

/// A joint source's stable identity: its two distinct physical cores, lower core first.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InteractionId {
    first: CoreId,
    second: CoreId,
}

impl InteractionId {
    pub fn new(first: CoreId, second: CoreId) -> Result<Self, TruthError> {
        match first.cmp(&second) {
            std::cmp::Ordering::Less => Ok(Self { first, second }),
            std::cmp::Ordering::Greater => Ok(Self {
                first: second,
                second: first,
            }),
            std::cmp::Ordering::Equal => Err(TruthError::Invalid("interaction endpoints")),
        }
    }

    /// The endpoints in ascending core order.
    pub const fn pair(self) -> (CoreId, CoreId) {
        (self.first, self.second)
    }

    /// Random-stream index: the lower core id in the high byte, the higher in the low byte.
    fn packed(self) -> u64 {
        (u64::from(self.first.get()) << 8) | u64::from(self.second.get())
    }
}

/// Expected failures of one joint source, kept per stable source id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InteractionExposure {
    pub source: InteractionId,
    pub expected_failures: f64,
}

/// Integrated competing hazards, in expected failures (not probabilities).
#[derive(Clone, Debug, PartialEq)]
pub struct Exposure {
    pub co_per_core: PerCore<f64>,
    /// One entry per registered joint pair, in registration order, zero when not loaded.
    pub interactions: Vec<InteractionExposure>,
    /// Platform background, a separate competing source that is never CO exposure.
    pub background: f64,
}

impl Exposure {
    pub fn interaction_total(&self) -> f64 {
        self.interactions
            .iter()
            .map(|entry| entry.expected_failures)
            .sum()
    }
    pub fn co_total(&self) -> f64 {
        self.co_per_core.iter().map(|(_, count)| count).sum::<f64>() + self.interaction_total()
    }
    pub fn failure_probability(&self) -> f64 {
        -(-(self.co_total() + self.background)).exp_m1()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureCause {
    Core(CoreId),
    /// A joint source, not attributed to either endpoint.
    Interaction(InteractionId),
    Background,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Failure {
    pub elapsed: Duration,
    pub cause: FailureCause,
}

/// What an observed prefix of a trial exposes. Intended duration is the class identity and is
/// never shortened by the prefix.
#[derive(Clone, Debug, PartialEq)]
pub struct SliceSeconds {
    pub intended: Duration,
    pub observed: Duration,
    /// Observed time past the warmup, measured on the uninterrupted exact-class-and-profile
    /// clock rather than from this slice's start.
    pub eligible: Duration,
    /// Observed time each loaded core's hazard is active after its own onset; unloaded cores
    /// are zero.
    pub onset_active: PerCore<Duration>,
}

#[derive(Clone, Copy, Debug)]
struct Source {
    cause: FailureCause,
    rate_per_hour: f64,
    after_s: f64,
}

impl Scenario {
    /// Seconds of uninterrupted exposure a core's CO hazard still waits for. Continuity is by
    /// exact class only, as a reference calendar knows no profile.
    pub(super) fn after_seconds(
        &self,
        class: &TrialClass,
        state: &TruthState,
        core: CoreId,
    ) -> Result<f64, TruthError> {
        self.onset(class, None, state, core)
    }

    fn onset(
        &self,
        class: &TrialClass,
        profile: Option<&Profile>,
        state: &TruthState,
        core: CoreId,
    ) -> Result<f64, TruthError> {
        if !self.topology().iter().any(|info| info.core == core) {
            return Err(TruthError::Invalid("core topology"));
        }
        if !class.loaded().contains(core) {
            return Ok(0.0);
        }
        let delay = || {
            ONSET_DELAY
                .saturating_sub(state.elapsed(class, profile))
                .as_secs_f64()
        };
        match self.fixture {
            Fixture::Legacy => match self.original.family {
                Family::Delayed => Ok(delay()),
                Family::Thermal => {
                    let heat = state.heat_of(core)?;
                    Ok(if heat >= THERMAL_THRESHOLD_S {
                        0.0
                    } else {
                        -THERMAL_TAU_S
                            * ((THERMAL_TAU_S - THERMAL_THRESHOLD_S) / (THERMAL_TAU_S - heat)).ln()
                    })
                }
                Family::Ordinary
                | Family::WriteLimitSafe
                | Family::FlatterDangerous
                | Family::Hidden
                | Family::Partial
                | Family::SingleCore
                | Family::Joint
                | Family::R8Nonbinding
                | Family::Interaction
                | Family::Moving
                | Family::Firmware => Ok(0.0),
            },
            Fixture::Delayed | Fixture::Combined => Ok(if self.weak.contains(&core) {
                delay()
            } else {
                0.0
            }),
            Fixture::Heterogeneous
            | Fixture::Hidden
            | Fixture::Interactions
            | Fixture::Refinement => Ok(0.0),
        }
    }

    fn check_exposure(
        &self,
        class: &TrialClass,
        profile: &Profile,
        state: &TruthState,
    ) -> Result<(), TruthError> {
        self.check_profile(profile)?;
        if state.topology.as_slice() != self.topology() {
            return Err(TruthError::Invalid("history topology"));
        }
        state.check_class(class)
    }

    /// The one source enumeration behind integration and draws, without allocating.
    fn for_each_source(
        &self,
        class: &TrialClass,
        profile: &Profile,
        state: &TruthState,
        day: u32,
        mut visit: impl FnMut(Source) -> Result<(), TruthError>,
    ) -> Result<(), TruthError> {
        if day == 0 {
            return Err(TruthError::Invalid("one-based calendar day"));
        }
        self.check_exposure(class, profile, state)?;
        let loaded = class.loaded();
        let depth_of = |core: CoreId| {
            profile
                .depths()
                .get(core)
                .ok_or(TruthError::Invalid("profile topology"))
        };
        for info in self.topology() {
            if !loaded.contains(info.core) {
                continue;
            }
            visit(Source {
                cause: FailureCause::Core(info.core),
                rate_per_hour: self.core_rate(class, info.core, depth_of(info.core)?, day)?,
                after_s: self.onset(class, Some(profile), state, info.core)?,
            })?;
        }
        for &(first, second) in self.joint_pairs() {
            if !loaded.contains(first) || !loaded.contains(second) {
                continue;
            }
            // Either endpoint at stock removes this product interaction.
            visit(Source {
                cause: FailureCause::Interaction(InteractionId::new(first, second)?),
                rate_per_hour: self.joint_root(depth_of(first)?)?
                    * self.joint_root(depth_of(second)?)?,
                after_s: 0.0,
            })?;
        }
        visit(Source {
            cause: FailureCause::Background,
            rate_per_hour: self.stock_background_per_hour(),
            after_s: 0.0,
        })
    }

    /// Expected failures of one run of `duration` actual time, which may exceed the class's
    /// intended duration when a calendar continues the same uninterrupted class.
    pub fn evaluate(
        &self,
        class: &TrialClass,
        profile: &Profile,
        state: &TruthState,
        duration: Duration,
        day: u32,
    ) -> Result<Exposure, TruthError> {
        // Every registered pair is present, even at zero, so provenance stays dense.
        let interactions = self
            .joint_pairs()
            .iter()
            .map(|&(first, second)| {
                InteractionId::new(first, second).map(|source| InteractionExposure {
                    source,
                    expected_failures: 0.0,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut exposure = Exposure {
            co_per_core: self
                .topology()
                .iter()
                .map(|info| (info.core, 0.0))
                .collect(),
            interactions,
            background: 0.0,
        };
        self.for_each_source(class, profile, state, day, |source| {
            let count =
                source.rate_per_hour * (duration.as_secs_f64() - source.after_s).max(0.0) / HOUR;
            if !count.is_finite() {
                return Err(TruthError::Invalid("unrepresentable exposure"));
            }
            match source.cause {
                FailureCause::Core(core) => {
                    if let Some(total) = exposure.co_per_core.get_mut(core) {
                        *total += count;
                    }
                }
                FailureCause::Interaction(id) => {
                    if let Some(entry) = exposure
                        .interactions
                        .iter_mut()
                        .find(|entry| entry.source == id)
                    {
                        entry.expected_failures += count;
                    }
                }
                FailureCause::Background => exposure.background += count,
            }
            Ok(())
        })?;
        Ok(exposure)
    }

    /// Account an observed prefix (a checkpoint or an interrupted run) of one trial.
    ///
    /// `warmup` is measured on the uninterrupted clock of exactly this class and profile, so
    /// earlier time of the same run counts toward it; it is not subtracted from every slice.
    pub fn slice_seconds(
        &self,
        class: &TrialClass,
        profile: &Profile,
        state: &TruthState,
        observed: Duration,
        warmup: Duration,
    ) -> Result<SliceSeconds, TruthError> {
        self.check_exposure(class, profile, state)?;
        let intended = class.duration();
        if observed > intended {
            return Err(TruthError::Invalid(
                "observed prefix beyond intended duration",
            ));
        }
        let waiting = warmup.saturating_sub(state.elapsed(class, Some(profile)));
        let loaded = class.loaded();
        let mut onset_active = PerCore::default();
        for info in self.topology() {
            let after =
                Duration::try_from_secs_f64(self.onset(class, Some(profile), state, info.core)?)
                    .map_err(|_| TruthError::Invalid("onset delay"))?;
            let active = if loaded.contains(info.core) {
                observed.saturating_sub(after)
            } else {
                Duration::ZERO
            };
            onset_active.set(info.core, active);
        }
        Ok(SliceSeconds {
            intended,
            observed,
            eligible: observed.saturating_sub(waiting),
            onset_active,
        })
    }

    /// Independent exponential clocks, keyed by seed, trial key, full class and source.
    /// Repeating the same arguments repeats the draw; no generator state is retained.
    #[expect(
        clippy::too_many_arguments,
        reason = "pure draw requires explicit exposure, history and random identity"
    )]
    pub fn draw(
        &self,
        class: &TrialClass,
        profile: &Profile,
        state: &TruthState,
        duration: Duration,
        day: u32,
        seed: u64,
        key: u64,
    ) -> Result<Option<Failure>, TruthError> {
        let mut first: Option<Failure> = None;
        let class_key = class_key(class);
        self.for_each_source(class, profile, state, day, |source| {
            if !source.rate_per_hour.is_finite() {
                return Err(TruthError::Invalid("unrepresentable exposure"));
            }
            if source.rate_per_hour == 0.0 {
                return Ok(());
            }
            let (domain, part) = match source.cause {
                FailureCause::Core(core) => (Domain::Core, u64::from(core.get())),
                FailureCause::Interaction(id) => (Domain::Joint(id.packed()), 0),
                FailureCause::Background => (Domain::Background, 0),
            };
            let mut rng = random::stream(seed, domain, &[STREAM_SALT, key, class_key, part]);
            let uniform: f64 = rng.random_range(f64::MIN_POSITIVE..1.0);
            let seconds = source.after_s - uniform.ln() * HOUR / source.rate_per_hour;
            if seconds.is_finite() && seconds < duration.as_secs_f64() {
                // A positive waiting time must advance the calendar, and a rounded event at
                // the end of the run is not before it.
                let elapsed = Duration::from_secs_f64(seconds).max(Duration::from_nanos(1));
                if elapsed < duration && first.is_none_or(|failure| elapsed < failure.elapsed) {
                    first = Some(Failure {
                        elapsed,
                        cause: source.cause,
                    });
                }
            }
            Ok(())
        })?;
        Ok(first)
    }
}

#[cfg(test)]
#[path = "exposure_tests.rs"]
mod tests;
