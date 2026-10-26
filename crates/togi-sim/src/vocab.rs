use togi_machine::{Regime, ResetKind, Signal};

/// The signals a simulated failure can draw, in draw order. `Signal::UncorrectedMce` is not among them:
/// it only arises from replayed facts.
pub const WEIGHTED_SIGNALS: [Signal; 5] = [
    Signal::ComputationError,
    Signal::Stall,
    Signal::UnexpectedExit,
    Signal::CorrectedMce,
    Signal::Crash,
];

/// The reset kinds a simulated crash can leave, in draw order.
pub const RESETS: [ResetKind; 7] = [
    ResetKind::Watchdog,
    ResetKind::SyncFlood,
    ResetKind::CpuShutdown,
    ResetKind::PowerButton,
    ResetKind::ThermalTrip,
    ResetKind::PowerLoss,
    ResetKind::Unknown,
];

/// How many regimes the simulator models: every one, R1 to R9.
pub(crate) const MODELED_REGIMES: usize = 9;

/// A regime's number in the simulator's draw keys. The numbers of R1 to R7 are part of the simulator's
/// recorded streams.
pub(crate) const fn regime_key(regime: Regime) -> u64 {
    regime_index(regime) as u64
}

/// Position of `regime` in the per-regime tables of a machine: R1 to R9 in order.
pub const fn regime_index(regime: Regime) -> usize {
    match regime {
        Regime::R1 => 0,
        Regime::R2 => 1,
        Regime::R3 => 2,
        Regime::R4 => 3,
        Regime::R5 => 4,
        Regime::R6 => 5,
        Regime::R7 => 6,
        Regime::R8 => 7,
        Regime::R9 => 8,
    }
}

/// The regime whose limits and, unless a model lists its own, signal mix `regime` takes: R1 to R7 themselves.
/// R8, a light role beside R2 neighbours on one CCD, is modeled as R2. R9, every thread of every loaded CCD,
/// is modeled as R7 at its loaded set.
pub(crate) const fn modeled_as(regime: Regime) -> Regime {
    match regime {
        Regime::R8 => Regime::R2,
        Regime::R9 => Regime::R7,
        Regime::R1
        | Regime::R2
        | Regime::R3
        | Regime::R4
        | Regime::R5
        | Regime::R6
        | Regime::R7 => regime,
    }
}

/// The position in a core's `alone` and `together` limits that `regime` reads: R1 to R7 their own, R8 R2's and
/// R9 R7's ([`modeled_as`]). A core's `workload` overrides still name R8 and R9 catalog entries exactly.
pub const fn limit_index(regime: Regime) -> usize {
    regime_index(modeled_as(regime))
}

pub(crate) const fn signal_slot(signal: Signal) -> Option<usize> {
    match signal {
        Signal::ComputationError => Some(0),
        Signal::Stall => Some(1),
        Signal::UnexpectedExit => Some(2),
        Signal::CorrectedMce => Some(3),
        Signal::Crash => Some(4),
        Signal::UncorrectedMce => None,
    }
}

pub(crate) const fn reset_slot(reset: ResetKind) -> usize {
    match reset {
        ResetKind::Watchdog => 0,
        ResetKind::SyncFlood => 1,
        ResetKind::CpuShutdown => 2,
        ResetKind::PowerButton => 3,
        ResetKind::ThermalTrip => 4,
        ResetKind::PowerLoss => 5,
        ResetKind::Unknown => 6,
    }
}

/// Validated signal weights: finite, nonnegative, positive total.
#[derive(Clone, Debug, PartialEq)]
pub struct SignalMix([f64; 5]);

impl SignalMix {
    pub(crate) const fn new(weights: [f64; 5]) -> Self {
        Self(weights)
    }

    pub fn weight(&self, signal: Signal) -> f64 {
        signal_slot(signal).map_or(0.0, |slot| self.0[slot])
    }

    pub fn total(&self) -> f64 {
        self.0.iter().sum()
    }

    /// The probability that a failure draws `signal`: its weight over the total, which unlike scaling a
    /// uniform draw by the total is exact for subnormal weights.
    pub fn probability(&self, signal: Signal) -> f64 {
        self.weight(signal) / self.total()
    }

    /// Maps `x` in `[0, 1)` to a signal in proportion to the weights.
    pub fn pick(&self, x: f64) -> Signal {
        pick(&WEIGHTED_SIGNALS, &self.0, x)
    }
}

/// Validated reset weights: finite, nonnegative, positive total.
#[derive(Clone, Debug, PartialEq)]
pub struct ResetMix([f64; 7]);

impl ResetMix {
    pub(crate) const fn new(weights: [f64; 7]) -> Self {
        Self(weights)
    }

    pub fn weight(&self, reset: ResetKind) -> f64 {
        self.0[reset_slot(reset)]
    }

    pub fn total(&self) -> f64 {
        self.0.iter().sum()
    }

    /// Maps `x` in `[0, 1)` to a reset in proportion to the weights.
    pub fn pick(&self, x: f64) -> ResetKind {
        pick(&RESETS, &self.0, x)
    }
}

/// Picks the kind at fraction `x` of the weights' total. The weights are compared in units of the
/// largest one, so a mix of subnormal weights draws in the same proportions as the same mix scaled up.
fn pick<T: Copy>(kinds: &[T], weights: &[f64], x: f64) -> T {
    let largest = weights.iter().copied().fold(0.0, f64::max);
    let mut left = x * weights.iter().map(|weight| weight / largest).sum::<f64>();
    let mut last = kinds[0];
    for (&kind, &weight) in kinds.iter().zip(weights) {
        let weight = weight / largest;
        if weight > 0.0 {
            if left < weight {
                return kind;
            }
            left -= weight;
            last = kind;
        }
    }
    last
}
