use serde::{Deserialize, Serialize};
use togi_machine::{Regime, ResetKind, Signal};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum RegimeId {
    R1,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
}

impl RegimeId {
    pub const ALL: [Self; 7] = [
        Self::R1,
        Self::R2,
        Self::R3,
        Self::R4,
        Self::R5,
        Self::R6,
        Self::R7,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::R1 => "R1",
            Self::R2 => "R2",
            Self::R3 => "R3",
            Self::R4 => "R4",
            Self::R5 => "R5",
            Self::R6 => "R6",
            Self::R7 => "R7",
        }
    }

    pub fn of(regime: &Regime) -> Option<Self> {
        match regime {
            Regime::R1 => Some(Self::R1),
            Regime::R2 => Some(Self::R2),
            Regime::R3 => Some(Self::R3),
            Regime::R4 => Some(Self::R4),
            Regime::R5 => Some(Self::R5),
            Regime::R6 => Some(Self::R6),
            Regime::R7 => Some(Self::R7),
            Regime::Empty | Regime::Unknown(_) => None,
        }
    }

    pub fn regime(self) -> Regime {
        match self {
            Self::R1 => Regime::R1,
            Self::R2 => Regime::R2,
            Self::R3 => Regime::R3,
            Self::R4 => Regime::R4,
            Self::R5 => Regime::R5,
            Self::R6 => Regime::R6,
            Self::R7 => Regime::R7,
        }
    }
}

/// The signals a simulated failure can draw, in draw order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    ComputationError,
    Stall,
    UnexpectedExit,
    CorrectedMce,
    Crash,
}

impl SignalKind {
    pub const ALL: [Self; 5] = [
        Self::ComputationError,
        Self::Stall,
        Self::UnexpectedExit,
        Self::CorrectedMce,
        Self::Crash,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ComputationError => "computation_error",
            Self::Stall => "stall",
            Self::UnexpectedExit => "unexpected_exit",
            Self::CorrectedMce => "corrected_mce",
            Self::Crash => "crash",
        }
    }

    pub fn of(signal: &Signal) -> Option<Self> {
        match signal {
            Signal::ComputationError => Some(Self::ComputationError),
            Signal::Stall => Some(Self::Stall),
            Signal::UnexpectedExit => Some(Self::UnexpectedExit),
            Signal::CorrectedMce => Some(Self::CorrectedMce),
            Signal::Crash => Some(Self::Crash),
            _ => None,
        }
    }

    pub fn signal(self) -> Signal {
        match self {
            Self::ComputationError => Signal::ComputationError,
            Self::Stall => Signal::Stall,
            Self::UnexpectedExit => Signal::UnexpectedExit,
            Self::CorrectedMce => Signal::CorrectedMce,
            Self::Crash => Signal::Crash,
        }
    }
}

/// The reset reasons a simulated crash can leave, in draw order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reset {
    Watchdog,
    SyncFlood,
    CpuShutdown,
    PowerButton,
    ThermalTrip,
    PowerLoss,
    Unknown,
}

impl Reset {
    pub const ALL: [Self; 7] = [
        Self::Watchdog,
        Self::SyncFlood,
        Self::CpuShutdown,
        Self::PowerButton,
        Self::ThermalTrip,
        Self::PowerLoss,
        Self::Unknown,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Watchdog => "watchdog",
            Self::SyncFlood => "sync_flood",
            Self::CpuShutdown => "cpu_shutdown",
            Self::PowerButton => "power_button",
            Self::ThermalTrip => "thermal_trip",
            Self::PowerLoss => "power_loss",
            Self::Unknown => "unknown",
        }
    }

    pub fn kind(self) -> ResetKind {
        match self {
            Self::Watchdog => ResetKind::Watchdog,
            Self::SyncFlood => ResetKind::SyncFlood,
            Self::CpuShutdown => ResetKind::CpuShutdown,
            Self::PowerButton => ResetKind::PowerButton,
            Self::ThermalTrip => ResetKind::ThermalTrip,
            Self::PowerLoss => ResetKind::PowerLoss,
            Self::Unknown => ResetKind::UnknownReason,
        }
    }
}

/// Validated signal weights: finite, nonnegative, positive total.
#[derive(Clone, Debug, PartialEq)]
pub struct SignalMix([f64; 5]);

impl SignalMix {
    pub(crate) fn new(weights: [f64; 5]) -> Self {
        Self(weights)
    }

    pub fn weight(&self, signal: SignalKind) -> f64 {
        self.0[signal.index()]
    }

    pub fn total(&self) -> f64 {
        self.0.iter().sum()
    }

    /// Maps `x` in `[0, 1)` to a signal in proportion to the weights.
    pub fn pick(&self, x: f64) -> SignalKind {
        pick(&SignalKind::ALL, &self.0, x)
    }
}

/// Validated reset weights: finite, nonnegative, positive total.
#[derive(Clone, Debug, PartialEq)]
pub struct ResetMix([f64; 7]);

impl ResetMix {
    pub(crate) fn new(weights: [f64; 7]) -> Self {
        Self(weights)
    }

    pub fn weight(&self, reset: Reset) -> f64 {
        self.0[reset.index()]
    }

    pub fn total(&self) -> f64 {
        self.0.iter().sum()
    }

    /// Maps `x` in `[0, 1)` to a reset in proportion to the weights.
    pub fn pick(&self, x: f64) -> Reset {
        pick(&Reset::ALL, &self.0, x)
    }
}

fn pick<T: Copy>(kinds: &[T], weights: &[f64], x: f64) -> T {
    let mut left = x * weights.iter().sum::<f64>();
    let mut last = kinds[0];
    for (&kind, &weight) in kinds.iter().zip(weights) {
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
