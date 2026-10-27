use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

macro_rules! vocabulary {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        pub enum $name { $(#[serde(rename = $wire)] $variant,)+ }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire,)+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
    };
}

vocabulary!(Regime { R1 => "R1", R2 => "R2", R3 => "R3", R4 => "R4", R5 => "R5", R6 => "R6", R7 => "R7", R8 => "R8", R9 => "R9" });

/// R1 to R7: the regimes without roles, and the research model's regime domain. [`ALL_REGIMES`] lists every regime.
pub const LEGACY_REGIMES: [Regime; 7] = [
    Regime::R1,
    Regime::R2,
    Regime::R3,
    Regime::R4,
    Regime::R5,
    Regime::R6,
    Regime::R7,
];

/// Every regime, in catalog order.
pub const ALL_REGIMES: [Regime; 9] = [
    Regime::R1,
    Regime::R2,
    Regime::R3,
    Regime::R4,
    Regime::R5,
    Regime::R6,
    Regime::R7,
    Regime::R8,
    Regime::R9,
];

/// The default duration of an R8 trial.
pub const R8_DURATION: Duration = Duration::from_secs(120);
/// The default duration of an R9 trial.
pub const R9_DURATION: Duration = Duration::from_secs(120);
/// How long a role regime's load warms up before its trial's telemetry summaries start counting.
pub const ROLE_WARM_UP: Duration = Duration::from_secs(30);
/// How many seconds each R9 phase lasts: all threads loaded, then one thread loaded.
pub const R9_PHASE_S: u32 = 10;
/// How long each R9 phase lasts; [`R9_PHASE_S`] as a duration.
pub const R9_PHASE: Duration = Duration::from_secs(10);

impl Regime {
    /// Whether every logical CPU runs the role its trial's roles give it: R8 and R9.
    pub const fn has_roles(self) -> bool {
        matches!(self, Self::R8 | Self::R9)
    }

    /// Whether the regime runs one instance per loaded core, each on the core's first logical CPU. R9 runs one
    /// instance per logical CPU, listed in the roles.
    pub const fn instance_per_core(self) -> bool {
        matches!(self, Self::R6 | Self::R7 | Self::R8)
    }

    /// The default duration of a role regime's trial; the legacy regimes' durations come from the configuration.
    pub const fn role_duration(self) -> Option<Duration> {
        match self {
            Self::R8 => Some(R8_DURATION),
            Self::R9 => Some(R9_DURATION),
            Self::R1 | Self::R2 | Self::R3 | Self::R4 | Self::R5 | Self::R6 | Self::R7 => None,
        }
    }

    /// The warm-up of a role regime's trial, whose telemetry summaries skip it; the legacy regimes have none.
    pub const fn warm_up(self) -> Option<Duration> {
        match self {
            Self::R8 | Self::R9 => Some(ROLE_WARM_UP),
            Self::R1 | Self::R2 | Self::R3 | Self::R4 | Self::R5 | Self::R6 | Self::R7 => None,
        }
    }
}

vocabulary!(Condition { Alone => "alone", Together => "together", Parked => "parked" });
vocabulary!(Signal { ComputationError => "computation_error", UnexpectedExit => "unexpected_exit", Stall => "stall", CorrectedMce => "corrected_mce", UncorrectedMce => "uncorrected_mce", Crash => "crash" });
vocabulary!(BankType { LoadStore => "load_store", InstructionFetch => "instruction_fetch", L2Cache => "l2_cache", DecodeUnit => "decode_unit", ExecutionUnit => "execution_unit", FloatingPoint => "floating_point", L3Cache => "l3_cache", MemoryController => "unified_memory_controller", DataFabric => "data_fabric", Other => "other", Unknown => "unknown" });

impl BankType {
    pub const fn core_local(self) -> bool {
        matches!(
            self,
            Self::LoadStore
                | Self::InstructionFetch
                | Self::L2Cache
                | Self::DecodeUnit
                | Self::ExecutionUnit
                | Self::FloatingPoint
        )
    }
}

vocabulary!(ResetKind { Watchdog => "watchdog", SyncFlood => "sync_flood", CpuShutdown => "cpu_shutdown", PowerButton => "power_button", ThermalTrip => "thermal_trip", PowerLoss => "power_loss", Unknown => "unknown" });
vocabulary!(Backend { Mprime => "mprime", Ycruncher => "ycruncher" });
