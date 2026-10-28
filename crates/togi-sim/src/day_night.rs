//! Opt-in calendar truth, independent of the session simulator and of tuning policy.
//!
//! Rates are per active hour. CO hazard is excess over the same class at CO 0; platform
//! background is a separate competing source. The same source enumeration drives integration
//! and draws. A caller advances history only by the exposure actually completed.

use std::time::Duration;

use rand::RngExt;
use togi_machine::{
    CcdId, Condition, CoreInfo, CoreSet, LoadSchedule, PerCore, PhaseSchedule, Regime,
    TargetOffset, TrialRole, TrialSpec, WorkloadId,
};

use crate::random::{self, Domain};

mod scenarios;
pub use scenarios::SCENARIO_IDS;

const HOUR: f64 = 3600.0;
const THERMAL_TAU_S: f64 = 1800.0;
const THERMAL_THRESHOLD_S: f64 = 600.0;
const REFERENCE_LAMBDA: f64 = 1.0 / 3000.0;

/// A refused truth input, before it can introduce non-finite hazard or invalid history.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TruthError {
    #[error("unknown day/night scenario {0:?}")]
    UnknownScenario(String),
    #[error("invalid day/night truth value: {0}")]
    Invalid(&'static str),
    #[error("construct day/night machine identity")]
    Identity,
    #[error("construct day/night trial roles")]
    Roles(#[source] togi_machine::InvalidRoles),
}

/// The complete experimental class, not a trial's label or offset assignment.
///
/// Workload catalog identity fixes content. Ordered cores, CPUs and roles fix placement,
/// process grouping, phase schedules and burst position. Intended duration remains part of
/// identity even when a calendar exposes only a prefix of a trial.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialClass {
    regime: Regime,
    workload: WorkloadId,
    condition: Condition,
    cores: Vec<togi_machine::CoreId>,
    cpus: Vec<togi_machine::CpuId>,
    roles: Vec<TrialRole>,
    duration: Duration,
}

impl TrialClass {
    pub fn from_spec(spec: &TrialSpec) -> Result<Self, TruthError> {
        if spec.duration.is_zero() || spec.roles.is_empty() || spec.workload.regime() != spec.regime
        {
            return Err(TruthError::Invalid("trial class"));
        }
        if TrialRole::cores(&spec.roles) != spec.cores || TrialRole::cpus(&spec.roles) != spec.cpus
        {
            return Err(TruthError::Invalid("trial placement"));
        }
        let loaded: CoreSet = spec.cores.iter().copied().collect();
        if loaded.len() != spec.cores.len() {
            return Err(TruthError::Invalid("duplicate core"));
        }
        Ok(Self {
            regime: spec.regime,
            workload: spec.workload,
            condition: spec.condition,
            cores: spec.cores.clone(),
            cpus: spec.cpus.clone(),
            roles: spec.roles.clone(),
            duration: spec.duration,
        })
    }

    /// Materialize a machine request without changing its class. A randomized phase's seed
    /// belongs to class identity, so a different requested schedule seed is refused.
    pub fn to_spec(
        &self,
        id: String,
        offsets: PerCore<TargetOffset>,
        index: usize,
        seed: u64,
    ) -> Result<TrialSpec, TruthError> {
        if self.roles.iter().any(|role| {
            matches!(role.schedule,
            PhaseSchedule::Steps { load: LoadSchedule::Random { seed: phase_seed } }
                if phase_seed != seed)
        }) {
            return Err(TruthError::Invalid("class phase seed"));
        }
        Ok(TrialSpec {
            id,
            regime: self.regime,
            workload: self.workload,
            condition: self.condition,
            cores: self.cores.clone(),
            cpus: self.cpus.clone(),
            roles: self.roles.clone(),
            duration: self.duration,
            index,
            seed,
            offsets,
            controller: togi_machine::ControllerMode::Managed {
                rotation: 0,
                purpose: togi_machine::Purpose::Exact,
            },
        })
    }

    pub const fn regime(&self) -> Regime {
        self.regime
    }
    pub const fn workload(&self) -> WorkloadId {
        self.workload
    }
    pub const fn condition(&self) -> Condition {
        self.condition
    }
    pub fn loaded(&self) -> CoreSet {
        self.cores.iter().copied().collect()
    }
    pub fn roles(&self) -> &[TrialRole] {
        &self.roles
    }
    pub const fn duration(&self) -> Duration {
        self.duration
    }

    /// Change intended duration, preserving every other part of the class.
    pub fn with_duration(&self, duration: Duration) -> Result<Self, TruthError> {
        if duration.is_zero() {
            return Err(TruthError::Invalid("trial duration"));
        }
        let mut class = self.clone();
        class.duration = duration;
        Ok(class)
    }

    fn loads_ccd(&self, ccd: usize) -> bool {
        self.cores
            .iter()
            .any(|core| usize::from(core.get()) / 8 == ccd)
    }

    fn loads_both_ccds(&self) -> bool {
        self.loads_ccd(0) && self.loads_ccd(1)
    }
}

/// A continuous exponential curve with an optional shallower asymptotic tail slope.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FailureCurve {
    edge_depth: f64,
    growth: f64,
    tail_growth: Option<f64>,
    edge_rate_per_hour: f64,
}

impl FailureCurve {
    pub fn new(
        edge_depth: f64,
        growth: f64,
        tail_growth: Option<f64>,
        edge_rate_per_hour: f64,
    ) -> Result<Self, TruthError> {
        if !edge_depth.is_finite()
            || !(-100.0..=100.0).contains(&edge_depth)
            || !growth.is_finite()
            || !(1.2..=6.0).contains(&growth)
            || !edge_rate_per_hour.is_finite()
            || edge_rate_per_hour < 0.0
        {
            return Err(TruthError::Invalid("failure curve"));
        }
        if tail_growth.is_some_and(|tail| !tail.is_finite() || !(1.0..=growth).contains(&tail)) {
            return Err(TruthError::Invalid("tail growth"));
        }
        let curve = Self {
            edge_depth,
            growth,
            tail_growth,
            edge_rate_per_hour,
        };
        if !curve.absolute_rate(50.0).is_finite() || !curve.absolute_rate(0.0).is_finite() {
            return Err(TruthError::Invalid("unrepresentable failure curve"));
        }
        Ok(curve)
    }

    fn absolute_rate(self, depth: f64) -> f64 {
        let distance = depth - self.edge_depth;
        let exponent = match self.tail_growth {
            None => distance * self.growth.ln(),
            Some(tail) => {
                let central = distance.clamp(-3.0, 3.0);
                central * self.growth.ln() + (distance - central) * tail.ln()
            }
        };
        self.edge_rate_per_hour * exponent.exp()
    }

    /// CO-caused rate above this class's CO-0 rate; exactly zero at stock.
    pub fn excess_per_hour(self, offset: TargetOffset) -> f64 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "checked offsets -50..=0 are exactly representable"
        )]
        let depth = -offset.get() as f64;
        (self.absolute_rate(depth) - self.absolute_rate(0.0)).max(0.0)
    }

    pub const fn growth(self) -> f64 {
        self.growth
    }
    pub const fn edge_depth(self) -> f64 {
        self.edge_depth
    }
}

/// An ordered part of the daily active load mix; fractions sum to one.
#[derive(Clone, Debug)]
pub struct DailyBlock {
    pub class: TrialClass,
    pub fraction: f64,
}

/// Explicit thermal memory and uninterrupted class exposure. Sleep cools but does not expose CO.
#[derive(Clone, Debug)]
pub struct TruthState {
    heat_s: Vec<f64>,
    continuous: Option<(TrialClass, f64)>,
}

impl TruthState {
    pub fn cold(ccd_count: usize) -> Result<Self, TruthError> {
        if !(1..=2).contains(&ccd_count) {
            return Err(TruthError::Invalid("CCD count"));
        }
        Ok(Self {
            heat_s: vec![0.0; ccd_count],
            continuous: None,
        })
    }

    /// Advance once by actual elapsed time, not intended trial duration. `None` means sleep.
    pub fn advance(
        &mut self,
        class: Option<&TrialClass>,
        duration: Duration,
    ) -> Result<(), TruthError> {
        let seconds = duration.as_secs_f64();
        if let Some(class) = class {
            if class
                .cores
                .iter()
                .any(|core| usize::from(core.get()) >= self.heat_s.len() * 8)
            {
                return Err(TruthError::Invalid("class topology"));
            }
            for (ccd, heat) in self.heat_s.iter_mut().enumerate() {
                let target = if class.loads_ccd(ccd) {
                    THERMAL_TAU_S
                } else {
                    0.0
                };
                *heat = target + (*heat - target) * (-seconds / THERMAL_TAU_S).exp();
            }
            match &mut self.continuous {
                Some((previous, elapsed)) if previous == class => *elapsed += seconds,
                Some(_) | None => self.continuous = Some((class.clone(), seconds)),
            }
        } else {
            for heat in &mut self.heat_s {
                *heat *= (-seconds / THERMAL_TAU_S).exp();
            }
            self.continuous = None;
        }
        Ok(())
    }

    pub fn heat_seconds(&self, ccd: CcdId) -> Option<f64> {
        self.heat_s.get(usize::from(ccd.get())).copied()
    }

    fn continuous_seconds(&self, class: &TrialClass) -> f64 {
        match &self.continuous {
            Some((previous, seconds)) if previous == class => *seconds,
            Some(_) | None => 0.0,
        }
    }
}

/// Integrated competing hazards, in expected failures (not probabilities).
#[derive(Clone, Debug, PartialEq)]
pub struct Exposure {
    pub co_per_ccd: Vec<f64>,
    pub interaction: f64,
    pub background: f64,
}

impl Exposure {
    pub fn co_total(&self) -> f64 {
        self.co_per_ccd.iter().sum::<f64>() + self.interaction
    }
    pub fn failure_probability(&self) -> f64 {
        -(-(self.co_total() + self.background)).exp_m1()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureCause {
    Ccd(CcdId),
    /// A joint source, not arbitrarily attributed to one CCD.
    Interaction,
    Background,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Failure {
    pub elapsed: Duration,
    pub cause: FailureCause,
}

/// The hidden subsystem behind a non-CPU interruption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NonCpuCause {
    Graphics,
    Power,
}

/// How the operator answers every non-CPU interruption of a scenario.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NonCpuAnswer {
    /// Correctly, as not the CPU.
    NotCpu,
    /// Wrongly, as the CPU.
    Cpu,
    /// Not at all.
    Missing,
}

/// The calendar days of a scenario's non-CPU interruptions, at most one a day, with their
/// subsystems.
const NON_CPU_DAYS: [(u32, NonCpuCause); 5] = [
    (30, NonCpuCause::Graphics),
    (120, NonCpuCause::Power),
    (240, NonCpuCause::Graphics),
    (400, NonCpuCause::Power),
    (600, NonCpuCause::Graphics),
];

/// A scenario's hidden non-CPU interruptions: graphics or power ends on fixed calendar days,
/// which the operator answers the same way each time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NonCpuSchedule {
    answer: NonCpuAnswer,
}

impl NonCpuSchedule {
    /// The subsystem that interrupts calendar `day`, if any.
    pub fn on(&self, day: u32) -> Option<NonCpuCause> {
        NON_CPU_DAYS
            .iter()
            .find_map(|&(scheduled, cause)| (scheduled == day).then_some(cause))
    }
    pub const fn answer(&self) -> NonCpuAnswer {
        self.answer
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Family {
    Ordinary,
    WriteLimitSafe,
    FlatterDangerous,
    Delayed,
    Thermal,
    Hidden,
    Partial,
    SingleCore,
    Joint,
    R8Nonbinding,
    Interaction,
    Moving,
    Firmware,
}

/// Hidden chip truth. Policies receive trial specifications, never this value or safe profiles.
#[derive(Clone, Debug)]
pub struct Scenario {
    id: &'static str,
    family: Family,
    topology: Vec<CoreInfo>,
    ccd_count: usize,
    edge: f64,
    growth: f64,
    tail: Option<f64>,
    background: f64,
    daily: [DailyBlock; 4],
    nights: Vec<TrialClass>,
    non_cpu: Option<NonCpuSchedule>,
}

#[derive(Clone, Copy)]
struct Source {
    cause: FailureCause,
    rate_per_hour: f64,
    after_s: f64,
}

impl Scenario {
    pub fn builtin(name: &str, seed: u64) -> Result<Self, TruthError> {
        scenarios::builtin(name, seed)
    }
    pub const fn id(&self) -> &'static str {
        self.id
    }
    pub const fn ccd_count(&self) -> usize {
        self.ccd_count
    }
    pub fn topology(&self) -> &[CoreInfo] {
        &self.topology
    }
    pub fn daily_mix(&self) -> &[DailyBlock] {
        &self.daily
    }
    pub fn night_classes(&self) -> &[TrialClass] {
        &self.nights
    }
    pub const fn stock_background_per_hour(&self) -> f64 {
        self.background
    }
    pub const fn transition_day(&self) -> Option<u32> {
        if matches!(self.family, Family::Firmware) {
            Some(180)
        } else {
            None
        }
    }
    pub const fn has_moving_cap(&self) -> bool {
        matches!(self.family, Family::Moving | Family::Firmware)
    }
    /// The hidden non-CPU interruptions, for the scenarios that have them.
    pub const fn non_cpu(&self) -> Option<NonCpuSchedule> {
        self.non_cpu
    }
    /// Truth epoch for memoizing metric-only safe profiles without duplicating the cap schedule.
    pub const fn cap_epoch(&self, day: u32) -> u32 {
        match self.family {
            Family::Moving => {
                let epoch = day / 45;
                if epoch < 10 { epoch } else { 10 }
            }
            Family::Firmware => {
                if day >= 180 {
                    1
                } else {
                    0
                }
            }
            Family::Ordinary
            | Family::WriteLimitSafe
            | Family::FlatterDangerous
            | Family::Delayed
            | Family::Thermal
            | Family::Hidden
            | Family::Partial
            | Family::SingleCore
            | Family::Joint
            | Family::R8Nonbinding
            | Family::Interaction => 0,
        }
    }
    pub const fn hidden_daily(&self) -> bool {
        matches!(self.family, Family::Hidden)
    }

    /// The joint curve of the interaction family's two CCDs; no other scenario has one.
    fn interaction_curve(&self) -> Result<Option<FailureCurve>, TruthError> {
        if self.family == Family::Interaction && self.ccd_count == 2 {
            FailureCurve::new(self.edge - 7.0, self.growth, self.tail, REFERENCE_LAMBDA).map(Some)
        } else {
            Ok(None)
        }
    }

    fn curve(&self, class: &TrialClass, ccd: usize, day: u32) -> Result<FailureCurve, TruthError> {
        let mut edge = self.edge;
        let mut growth = self.growth;
        let all = class
            .cores
            .iter()
            .filter(|core| usize::from(core.get()) / 8 == ccd)
            .count();
        match self.family {
            Family::Ordinary | Family::Delayed | Family::Thermal => {}
            Family::WriteLimitSafe => return FailureCurve::new(edge, growth, self.tail, 0.0),
            Family::FlatterDangerous => {
                if class.regime == Regime::R2 {
                    edge -= 10.0;
                    growth = 1.2;
                }
            }
            Family::Hidden => {
                if class.workload == scenarios::workload(Regime::R2, 2)? {
                    edge -= 10.0;
                }
            }
            Family::Partial => {
                let partial = class.regime == Regime::R7 && (2..8).contains(&all);
                edge += if partial { -8.0 } else { 15.0 };
            }
            Family::SingleCore => {
                let binding = class.regime == Regime::R1
                    && all == 1
                    && class
                        .cores
                        .iter()
                        .any(|core| usize::from(core.get()) == ccd * 8);
                edge += if binding { -8.0 } else { 12.0 };
            }
            Family::Joint => {
                edge += if all >= 2 { -7.0 } else { 12.0 };
            }
            Family::R8Nonbinding => {
                if class.regime == Regime::R8 {
                    edge += 18.0;
                }
            }
            Family::Interaction => {
                edge += 15.0;
            }
            Family::Moving => {
                edge -= f64::from((day / 45).min(10));
            }
            Family::Firmware => {
                if day >= 180 {
                    edge -= 8.0;
                }
            }
        }
        // Apart from explicit binding families, full sustained load is the reference class.
        if matches!(
            self.family,
            Family::Ordinary
                | Family::Delayed
                | Family::Thermal
                | Family::Hidden
                | Family::FlatterDangerous
                | Family::Moving
                | Family::Firmware
        ) && class.regime != Regime::R7
            && class.regime != Regime::R2
        {
            edge += 5.0;
        }
        FailureCurve::new(
            edge,
            growth,
            self.tail,
            REFERENCE_LAMBDA / count_as_float(self.ccd_count),
        )
    }

    fn check(
        &self,
        class: &TrialClass,
        offsets: &[TargetOffset],
        state: &TruthState,
    ) -> Result<(), TruthError> {
        if offsets.len() != self.ccd_count || state.heat_s.len() != self.ccd_count {
            return Err(TruthError::Invalid("profile or history CCD count"));
        }
        if class
            .cores
            .iter()
            .any(|core| usize::from(core.get()) >= self.ccd_count * 8)
        {
            return Err(TruthError::Invalid("class topology"));
        }
        Ok(())
    }

    fn after_seconds(&self, class: &TrialClass, state: &TruthState, ccd: usize) -> f64 {
        match self.family {
            Family::Delayed => (600.0 - state.continuous_seconds(class)).max(0.0),
            Family::Thermal => {
                let heat = state.heat_s[ccd];
                if heat >= THERMAL_THRESHOLD_S {
                    0.0
                } else {
                    -THERMAL_TAU_S
                        * ((THERMAL_TAU_S - THERMAL_THRESHOLD_S) / (THERMAL_TAU_S - heat)).ln()
                }
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
            | Family::Firmware => 0.0,
        }
    }

    fn sources(
        &self,
        class: &TrialClass,
        offsets: &[TargetOffset],
        state: &TruthState,
        day: u32,
    ) -> Result<[Option<Source>; 4], TruthError> {
        self.check(class, offsets, state)?;
        let mut sources = [None; 4];
        for (ccd, &offset) in offsets.iter().enumerate() {
            if !class.loads_ccd(ccd) {
                continue;
            }
            let after_s = self.after_seconds(class, state, ccd);
            let id = CcdId::new(i64::try_from(ccd).map_err(|_| TruthError::Identity)?)
                .map_err(|_| TruthError::Identity)?;
            sources[ccd] = Some(Source {
                cause: FailureCause::Ccd(id),
                rate_per_hour: self.curve(class, ccd, day)?.excess_per_hour(offset),
                after_s,
            });
        }
        if class.loads_both_ccds()
            && let Some(joint) = self.interaction_curve()?
        {
            // Product of the two CCD voltage deficits: either stock CCD removes the interaction.
            sources[2] = Some(Source {
                cause: FailureCause::Interaction,
                rate_per_hour: joint.excess_per_hour(offsets[0]).sqrt()
                    * joint.excess_per_hour(offsets[1]).sqrt(),
                after_s: 0.0,
            });
        }
        sources[3] = Some(Source {
            cause: FailureCause::Background,
            rate_per_hour: self.background,
            after_s: 0.0,
        });
        Ok(sources)
    }

    pub fn evaluate(
        &self,
        class: &TrialClass,
        offsets: &[TargetOffset],
        state: &TruthState,
        duration: Duration,
        day: u32,
    ) -> Result<Exposure, TruthError> {
        let mut exposure = Exposure {
            co_per_ccd: vec![0.0; self.ccd_count],
            interaction: 0.0,
            background: 0.0,
        };
        for source in self
            .sources(class, offsets, state, day)?
            .into_iter()
            .flatten()
        {
            let count =
                source.rate_per_hour * (duration.as_secs_f64() - source.after_s).max(0.0) / HOUR;
            if !count.is_finite() {
                return Err(TruthError::Invalid("unrepresentable exposure"));
            }
            match source.cause {
                FailureCause::Ccd(ccd) => exposure.co_per_ccd[usize::from(ccd.get())] += count,
                FailureCause::Interaction => exposure.interaction += count,
                FailureCause::Background => exposure.background += count,
            }
        }
        Ok(exposure)
    }

    /// Independent exponential clocks, keyed by seed, trial key, full class and source.
    /// Repeating the same arguments repeats the draw; no random generator state is retained.
    #[expect(
        clippy::too_many_arguments,
        reason = "pure draw requires explicit exposure, history and random identity"
    )]
    pub fn draw(
        &self,
        class: &TrialClass,
        offsets: &[TargetOffset],
        state: &TruthState,
        duration: Duration,
        day: u32,
        seed: u64,
        key: u64,
    ) -> Result<Option<Failure>, TruthError> {
        let mut first = None;
        let class_key = class_key(class);
        for source in self
            .sources(class, offsets, state, day)?
            .into_iter()
            .flatten()
        {
            if source.rate_per_hour == 0.0 {
                continue;
            }
            let domain = match source.cause {
                FailureCause::Ccd(ccd) => Domain::Ccd(ccd.get()),
                FailureCause::Interaction => Domain::Joint(0),
                FailureCause::Background => Domain::Background,
            };
            let mut rng = random::stream(seed, domain, &[0x4441_594e_4947_4854, key, class_key]);
            let uniform: f64 = rng.random_range(f64::MIN_POSITIVE..1.0);
            let seconds = source.after_s - uniform.ln() * HOUR / source.rate_per_hour;
            if seconds.is_finite() && seconds < duration.as_secs_f64() {
                // Duration has nanosecond resolution: a positive waiting time must advance
                // the calendar, and a rounded event at the intended end is not before it.
                let elapsed = Duration::from_secs_f64(seconds).max(Duration::from_nanos(1));
                if elapsed >= duration {
                    continue;
                }
                if first
                    .as_ref()
                    .is_none_or(|failure: &Failure| elapsed < failure.elapsed)
                {
                    first = Some(Failure {
                        elapsed,
                        cause: source.cause,
                    });
                }
            }
        }
        Ok(first)
    }

    /// Deepest uniform-per-CCD profile under individual lambda/n and joint lambda limits.
    ///
    /// Integrates the canonical eight-hour active day, in daily-block order, and sixteen
    /// hours asleep, at the exact periodic thermal fixed point. Night stress is deliberately
    /// excluded from this policy-independent reference. Delayed onset and actual class
    /// durations are integrated; background is excluded. Among feasible profiles, maximize
    /// total retained depth, then minimize CCD imbalance, then lexicographically smaller
    /// depths. A joint scenario may not have a componentwise deepest feasible profile.
    pub fn safe_profile(
        &self,
        day: u32,
        lambda_per_hour: f64,
    ) -> Result<Vec<TargetOffset>, TruthError> {
        let fractions = self.daily.each_ref().map(|block| block.fraction);
        self.safe_profile_for(day, lambda_per_hour, Duration::from_hours(8), &fractions)
    }

    /// The same policy-independent reference with configured active duration and load mix.
    /// Fractions follow `daily_mix` order; the remainder of the 24-hour cycle is asleep.
    pub fn safe_profile_for(
        &self,
        day: u32,
        lambda_per_hour: f64,
        active: Duration,
        fractions: &[f64],
    ) -> Result<Vec<TargetOffset>, TruthError> {
        if !lambda_per_hour.is_finite() || lambda_per_hour <= 0.0 {
            return Err(TruthError::Invalid("lambda per hour"));
        }
        let cycle = Duration::from_hours(24);
        if active.is_zero()
            || active > cycle
            || fractions.len() != self.daily.len()
            || fractions
                .iter()
                .any(|fraction| !fraction.is_finite() || *fraction < 0.0)
        {
            return Err(TruthError::Invalid("daily reference calendar"));
        }
        let sum = fractions.iter().sum::<f64>();
        if !sum.is_finite() || (sum - 1.0).abs() > 1e-12 {
            return Err(TruthError::Invalid("daily reference fractions"));
        }
        let mut durations = [Duration::ZERO; 4];
        let mut remaining = active;
        for (index, duration) in durations.iter_mut().enumerate() {
            *duration = if index == 3 {
                remaining
            } else {
                active.mul_f64(fractions[index] / sum).min(remaining)
            };
            remaining = remaining.saturating_sub(*duration);
        }
        let sleep = cycle
            .checked_sub(active)
            .ok_or(TruthError::Invalid("daily reference calendar"))?;
        let mut state = TruthState::cold(self.ccd_count)?;
        for (block, &duration) in self.daily.iter().zip(&durations) {
            if !duration.is_zero() {
                state.advance(Some(&block.class), duration)?;
            }
        }
        if !sleep.is_zero() {
            state.advance(None, sleep)?;
        }
        // One cold cycle yields b in h_next = a*h_start+b; solve the recurrence exactly.
        let recurrence = (-24.0 * HOUR / THERMAL_TAU_S).exp();
        for heat in &mut state.heat_s {
            *heat /= 1.0 - recurrence;
        }
        let mut rates = [[0.0; 51]; 2];
        for (block, &duration) in self.daily.iter().zip(&durations) {
            if duration.is_zero() {
                continue;
            }
            for (ccd, ccd_rates) in rates.iter_mut().enumerate().take(self.ccd_count) {
                if !block.class.loads_ccd(ccd) {
                    continue;
                }
                let curve = self.curve(&block.class, ccd, day)?;
                let exposed_seconds = (duration.as_secs_f64()
                    - self.after_seconds(&block.class, &state, ccd))
                .max(0.0);
                let weight = exposed_seconds / active.as_secs_f64();
                for (depth, rate) in ccd_rates.iter_mut().enumerate() {
                    *rate += weight * curve.excess_per_hour(offset_for_depth(depth)?);
                }
            }
            state.advance(Some(&block.class), duration)?;
        }
        let mut joint_roots = [0.0; 51];
        let joint_fraction = if let Some(joint) = self.interaction_curve()? {
            for (depth, root) in joint_roots.iter_mut().enumerate() {
                *root = joint.excess_per_hour(offset_for_depth(depth)?).sqrt();
            }
            self.daily
                .iter()
                .zip(&durations)
                .filter(|(block, _)| block.class.loads_both_ccds())
                .map(|(_, duration)| duration.as_secs_f64() / active.as_secs_f64())
                .sum()
        } else {
            0.0
        };
        let share = lambda_per_hour / count_as_float(self.ccd_count);
        let mut best = (0_usize, 0_usize);
        let mut best_score = (0_usize, usize::MAX);
        for first in 0..=50 {
            if rates[0][first] > share {
                continue;
            }
            for second in 0..=if self.ccd_count == 2 { 50 } else { 0 } {
                if self.ccd_count == 2 && rates[1][second] > share {
                    continue;
                }
                let total = rates[0][first]
                    + if self.ccd_count == 2 {
                        rates[1][second]
                    } else {
                        0.0
                    }
                    + joint_fraction * joint_roots[first] * joint_roots[second];
                if total > lambda_per_hour {
                    continue;
                }
                let score = (first + second, first.abs_diff(second));
                if score.0 > best_score.0 || (score.0 == best_score.0 && score.1 < best_score.1) {
                    best = (first, second);
                    best_score = score;
                }
            }
        }
        let mut profile = vec![offset_for_depth(best.0)?];
        if self.ccd_count == 2 {
            profile.push(offset_for_depth(best.1)?);
        }
        Ok(profile)
    }
}

fn offset_for_depth(depth: usize) -> Result<TargetOffset, TruthError> {
    let depth = i64::try_from(depth).map_err(|_| TruthError::Identity)?;
    TargetOffset::new(-depth).map_err(|_| TruthError::Identity)
}

fn count_as_float(count: usize) -> f64 {
    if count == 1 { 1.0 } else { 2.0 }
}

fn class_key(class: &TrialClass) -> u64 {
    fn absorb(state: u64, part: u64) -> u64 {
        (state ^ part)
            .rotate_left(17)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }
    let condition = match class.condition {
        Condition::Alone => 1,
        Condition::Together => 2,
        Condition::Parked => 3,
    };
    let mut key = absorb(random::text_part(class.workload.as_str()), condition);
    key = absorb(key, class.duration.as_secs());
    key = absorb(key, u64::from(class.duration.subsec_nanos()));
    for role in &class.roles {
        key = absorb(key, u64::from(role.core.get()));
        key = absorb(key, u64::from(role.cpu.get()));
        key = absorb(key, u64::from(role.instance.get()));
        key = absorb(key, random::text_part(role.workload.as_str()));
        match role.schedule {
            PhaseSchedule::Sustained {} => key = absorb(key, 1),
            PhaseSchedule::Steps { load } => {
                key = absorb(key, 2);
                match load {
                    LoadSchedule::Random { seed } => {
                        key = absorb(key, 1);
                        key = absorb(key, seed);
                    }
                    LoadSchedule::Duty { pct } => {
                        key = absorb(key, 2);
                        key = absorb(key, u64::from(pct.get()));
                    }
                }
            }
            PhaseSchedule::Burst { position } => {
                key = absorb(key, 3);
                key = absorb(key, position as u64);
            }
            PhaseSchedule::Alternate { phase_s, serial } => {
                key = absorb(key, 4);
                key = absorb(key, u64::from(phase_s));
                key = absorb(key, u64::from(serial));
            }
        }
    }
    key
}

#[cfg(test)]
#[path = "day_night_tests.rs"]
mod tests;
