use crate::go_json::write_json_string;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Defines a wire vocabulary with one value per string, as in Go: every named
/// string, the empty one included, has its own variant, and `Unknown` holds
/// only strings no variant names. The wrapper's only constructor is the
/// normalizing `From`, so a named or empty string cannot be stored in it.
macro_rules! vocabulary {
    ($name:ident, $unknown:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[doc = concat!(
            "A wire string no variant of [`", stringify!($name), "`] names.\n\n",
            "The only constructor is `From`, so a named or empty string cannot be stored in it:\n\n",
            "```compile_fail,E0423\n",
            "let _ = togi_machine::", stringify!($name), "::Unknown(togi_machine::", stringify!($unknown), "(String::new()));\n",
            "```"
        )]
        #[derive(Clone, Debug, Eq, Hash, PartialEq)]
        pub struct $unknown(String);

        impl $unknown {
            pub fn as_str(&self) -> &str { &self.0 }
        }

        #[derive(Clone, Debug, Eq, Hash, PartialEq)]
        pub enum $name { $($variant,)+ Unknown($unknown) }

        impl $name {
            pub fn as_str(&self) -> &str {
                match self { $(Self::$variant => $wire,)+ Self::Unknown(value) => value.as_str() }
            }

            /// The JSON string Go writes for the value; see `HostText::json`.
            pub fn json(&self, escape_html: bool) -> String {
                let mut result = String::new();
                write_json_string(&mut result, self.as_str().as_bytes(), escape_html);
                result
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value { $($wire => Self::$variant,)+ value => Self::Unknown($unknown(value.to_owned())) }
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                match value.as_str() { $($wire => Self::$variant,)+ _ => Self::Unknown($unknown(value)) }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serde_json::value::RawValue::from_string(self.json(true))
                    .map_err(serde::ser::Error::custom)?
                    .serialize(serializer)
            }
        }

        /// Decodes a string, or `null` as the empty value. Bytes Go wrote go
        /// through `json_text` first.
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

vocabulary!(Regime, UnknownRegime { R1 => "R1", R2 => "R2", R3 => "R3", R4 => "R4", R5 => "R5", R6 => "R6", R7 => "R7", Empty => "" });
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

vocabulary!(Condition, UnknownCondition { Alone => "alone", Together => "together", Parked => "parked", Empty => "" });
vocabulary!(Signal, UnknownSignal { ComputationError => "computation_error", UnexpectedExit => "unexpected_exit", Stall => "stall", CorrectedMce => "corrected_mce", UncorrectedMce => "uncorrected_mce", Crash => "crash", Empty => "" });
vocabulary!(BankType, UnknownBankType { LoadStore => "load_store", InstructionFetch => "instruction_fetch", L2Cache => "l2_cache", DecodeUnit => "decode_unit", ExecutionUnit => "execution_unit", FloatingPoint => "floating_point", L3Cache => "l3_cache", MemoryController => "unified_memory_controller", DataFabric => "data_fabric", Other => "other", UnknownBank => "unknown", Empty => "" });

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

vocabulary!(ResetKind, UnknownResetKind { Watchdog => "watchdog", SyncFlood => "sync_flood", CpuShutdown => "cpu_shutdown", PowerButton => "power_button", ThermalTrip => "thermal_trip", PowerLoss => "power_loss", UnknownReason => "unknown", Empty => "" });
vocabulary!(Backend, UnknownBackend { Mprime => "mprime", Ycruncher => "ycruncher", Empty => "" });
