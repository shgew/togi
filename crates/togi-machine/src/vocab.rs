use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

macro_rules! vocabulary {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Clone, Debug, Eq, Hash, PartialEq)]
        pub enum $name { $($variant,)+ Unknown(String) }

        impl $name {
            pub fn as_str(&self) -> &str {
                match self { $(Self::$variant => $wire,)+ Self::Unknown(value) => value }
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value { $($wire => Self::$variant,)+ value => Self::Unknown(value.to_owned()) }
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                match value.as_str() { $($wire => Self::$variant,)+ _ => Self::Unknown(value) }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Vocabulary;
                impl<'de> serde::de::Visitor<'de> for Vocabulary {
                    type Value = $name;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("a vocabulary string or null") }
                    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<$name, E> { Ok($name::from(value)) }
                    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<$name, E> { Ok($name::from(value)) }
                    fn visit_unit<E: serde::de::Error>(self) -> Result<$name, E> { Ok($name::from("")) }
                }
                deserializer.deserialize_any(Vocabulary)
            }
        }
    };
}

vocabulary!(Regime { R1 => "R1", R2 => "R2", R3 => "R3", R4 => "R4", R5 => "R5", R6 => "R6", R7 => "R7", Empty => "" });
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
    pub const fn valid(&self) -> bool {
        match self {
            Self::R1 | Self::R2 | Self::R3 | Self::R4 | Self::R5 | Self::R6 | Self::R7 => true,
            Self::Empty | Self::Unknown(_) => false,
        }
    }

    pub const fn instance_per_core(&self) -> bool {
        match self {
            Self::R6 | Self::R7 => true,
            Self::R1
            | Self::R2
            | Self::R3
            | Self::R4
            | Self::R5
            | Self::Empty
            | Self::Unknown(_) => false,
        }
    }
}

vocabulary!(Condition { Alone => "alone", Together => "together", Parked => "parked", Empty => "" });
vocabulary!(Signal { ComputationError => "computation_error", UnexpectedExit => "unexpected_exit", Stall => "stall", CorrectedMce => "corrected_mce", UncorrectedMce => "uncorrected_mce", Crash => "crash", Empty => "" });
vocabulary!(BankType { LoadStore => "load_store", InstructionFetch => "instruction_fetch", L2Cache => "l2_cache", DecodeUnit => "decode_unit", ExecutionUnit => "execution_unit", FloatingPoint => "floating_point", L3Cache => "l3_cache", MemoryController => "unified_memory_controller", DataFabric => "data_fabric", Other => "other", UnknownBank => "unknown", Empty => "" });

impl BankType {
    pub const fn core_local(&self) -> bool {
        match self {
            Self::LoadStore
            | Self::InstructionFetch
            | Self::L2Cache
            | Self::DecodeUnit
            | Self::ExecutionUnit
            | Self::FloatingPoint => true,
            Self::L3Cache
            | Self::MemoryController
            | Self::DataFabric
            | Self::Other
            | Self::UnknownBank
            | Self::Empty
            | Self::Unknown(_) => false,
        }
    }
}

vocabulary!(ResetKind { Watchdog => "watchdog", SyncFlood => "sync_flood", CpuShutdown => "cpu_shutdown", PowerButton => "power_button", ThermalTrip => "thermal_trip", PowerLoss => "power_loss", UnknownReason => "unknown", Empty => "" });
vocabulary!(Backend { Mprime => "mprime", Ycruncher => "ycruncher", Empty => "" });
