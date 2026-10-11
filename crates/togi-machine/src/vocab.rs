use serde::{Deserialize, Serialize};
use std::fmt;

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

vocabulary!(Regime { R1 => "R1", R2 => "R2", R3 => "R3", R4 => "R4", R5 => "R5", R6 => "R6", R7 => "R7" });

pub const REGIMES: [Regime; 7] = [
    Regime::R1,
    Regime::R2,
    Regime::R3,
    Regime::R4,
    Regime::R5,
    Regime::R6,
    Regime::R7,
];

impl Regime {
    pub const fn instance_per_core(self) -> bool {
        matches!(self, Self::R6 | Self::R7)
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
