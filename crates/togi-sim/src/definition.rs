//! The machine-file document as serde types.
//!
//! Decoding is strict: unknown fields, duplicate keys (in maps too), wrong array lengths and unsupported
//! signal, reset and regime names are errors. A field equal to its default is omitted when encoding, and
//! an omitted field decodes to its default, so `decode(encode(d)) == d` for every valid `d`.
//! [`Model::new`](crate::Model::new) checks the values.

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use togi_machine::{Regime, ResetKind, Signal};

pub const DEFAULT_CORES: i64 = 16;
pub const DEFAULT_PAST_LIMIT_RATE: f64 = std::f64::consts::LN_10 / 90.0;
pub const DEFAULT_GROWTH: f64 = 4.0;
pub const DEFAULT_CRASH_MCE: f64 = 0.5;
pub const DEFAULT_CORE_LOCAL_BANK: f64 = 0.8;
pub const DEFAULT_ONSET_S: f64 = 100.0;
/// Volts per count of CO offset when a shared-voltage core leaves `count_v` at 0.
pub const DEFAULT_COUNT_V: f64 = 0.0036;

pub const DEFAULT_BIOS_VERSION: &str = "SIM.1";
pub const DEFAULT_BOARD: &str = "togi simulator";
pub const DEFAULT_CPU_MODEL: &str = "Simulated Zen 5 16-Core Processor";
pub const DEFAULT_MICROCODE: &str = "0x0";
pub const DEFAULT_BOOST_LIMIT_MHZ: u32 = 5700;

/// Decodes a struct from a JSON object only. Derived `Deserialize` also accepts a sequence of the fields in
/// order, which a machine file must not. The struct derives with `remote = "Self"`, which leaves the
/// derived functions inherent, and this macro makes them the trait implementation.
macro_rules! object {
    ($($ty:ident),+ $(,)?) => {$(
        impl<'de> ::serde::Deserialize<'de> for $ty {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Object;

                impl<'de> ::serde::de::Visitor<'de> for Object {
                    type Value = $ty;

                    fn expecting(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                        f.write_str(concat!("a ", stringify!($ty), " object"))
                    }

                    fn visit_map<A: ::serde::de::MapAccess<'de>>(self, map: A) -> Result<$ty, A::Error> {
                        $ty::deserialize(::serde::de::value::MapAccessDeserializer::new(map))
                    }
                }

                deserializer.deserialize_map(Object)
            }
        }
    )+};
}

macro_rules! inherent_serialize {
    ($($ty:ident),+ $(,)?) => {$(
        impl ::serde::Serialize for $ty {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                $ty::serialize(self, serializer)
            }
        }
    )+};
}

pub(crate) use object;

/// A map of weights keyed by a closed vocabulary, with duplicate keys refused on decode.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Weights<K: Ord>(pub BTreeMap<K, f64>);

impl<K: Ord> Default for Weights<K> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<K: Ord, const N: usize> From<[(K, f64); N]> for Weights<K> {
    fn from(weights: [(K, f64); N]) -> Self {
        Self(weights.into_iter().collect())
    }
}

impl<'de, K: Deserialize<'de> + Ord + fmt::Debug> Deserialize<'de> for Weights<K> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        unique_map(deserializer).map(Self)
    }
}

/// Decodes a map and refuses a key that appears twice, which `BTreeMap` would silently overwrite.
pub fn unique_map<'de, D, K, V>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
where
    D: Deserializer<'de>,
    K: Deserialize<'de> + Ord + fmt::Debug,
    V: Deserialize<'de>,
{
    struct Unique<K, V>(PhantomData<(K, V)>);

    impl<'de, K, V> Visitor<'de> for Unique<K, V>
    where
        K: Deserialize<'de> + Ord + fmt::Debug,
        V: Deserialize<'de>,
    {
        type Value = BTreeMap<K, V>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a map")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<K, V>()? {
                if out.contains_key(&key) {
                    return Err(de::Error::custom(format_args!("duplicate key {key:?}")));
                }
                out.insert(key, value);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_map(Unique(PhantomData))
}

/// Predicates for `skip_serializing_if`, which serde calls with a reference to the field.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde calls skip_serializing_if predicates with a reference"
)]
mod skip {
    use super::{
        DEFAULT_CORE_LOCAL_BANK, DEFAULT_CORES, DEFAULT_CRASH_MCE, DEFAULT_GROWTH, DEFAULT_ONSET_S,
        DEFAULT_PAST_LIMIT_RATE, ResetKind, Signal, Weights, default_reset, default_signals,
    };

    pub(super) const fn is_zero(value: &f64) -> bool {
        *value == 0.0
    }

    pub(super) const fn is_zero_core(value: &i64) -> bool {
        *value == 0
    }

    pub(super) const fn is_false(value: &bool) -> bool {
        !*value
    }

    pub(super) const fn cores(value: &i64) -> bool {
        *value == DEFAULT_CORES
    }

    pub(super) const fn past_limit_rate(value: &f64) -> bool {
        *value == DEFAULT_PAST_LIMIT_RATE
    }

    pub(super) const fn growth(value: &f64) -> bool {
        *value == DEFAULT_GROWTH
    }

    pub(super) const fn crash_mce(value: &f64) -> bool {
        *value == DEFAULT_CRASH_MCE
    }

    pub(super) const fn core_local_bank(value: &f64) -> bool {
        *value == DEFAULT_CORE_LOCAL_BANK
    }

    pub(super) const fn onset_s(value: &f64) -> bool {
        *value == DEFAULT_ONSET_S
    }

    pub(super) fn signals(weights: &Weights<Signal>) -> bool {
        *weights == default_signals()
    }

    pub(super) fn reset(weights: &Weights<ResetKind>) -> bool {
        *weights == default_reset()
    }
}

/// A machine file: everything that makes a simulated machine, and nothing about a particular run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields, default)]
pub struct MachineDefinition {
    /// What a hand-written machine models. Metadata only.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Provenance of a generated machine. Metadata only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// Even, 2 to 16. Core `c` is on CCD `c / (cores / 2)`.
    #[serde(skip_serializing_if = "skip::cores")]
    pub cores: i64,
    /// Path of the trial-facts extract, relative to this file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facts: Option<String>,
    /// Offsets restored at every boot; all zero when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bios: Option<Vec<i64>>,
    /// Preferred-core ranking, one value per core; unavailable when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ranking: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "skip::is_false")]
    pub old_kernel: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bios_context: Option<BiosContextDef>,
    #[serde(skip_serializing_if = "ModelDef::is_default")]
    pub model: ModelDef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ccd: Option<CcdDef>,
    /// Hidden limits of every core, in any order; the seed draws them when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub core: Vec<CoreDef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub joint: Vec<JointDef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub script: Vec<ScriptDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_voltage: Option<SharedVoltageDef>,
}

impl Default for MachineDefinition {
    fn default() -> Self {
        Self {
            description: String::new(),
            notes: Vec::new(),
            cores: DEFAULT_CORES,
            facts: None,
            bios: None,
            ranking: None,
            old_kernel: false,
            bios_context: None,
            model: ModelDef::default(),
            ccd: None,
            core: Vec::new(),
            joint: Vec::new(),
            script: Vec::new(),
            shared_voltage: None,
        }
    }
}

/// Fields left out take the simulator's default BIOS context.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields, default)]
pub struct BiosContextDef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bios_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub microcode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boost_limit_mhz: Option<u32>,
}

fn default_signals() -> Weights<Signal> {
    Weights::from([
        (Signal::ComputationError, 4.0),
        (Signal::Stall, 1.0),
        (Signal::UnexpectedExit, 1.0),
        (Signal::CorrectedMce, 2.0),
        (Signal::Crash, 2.0),
    ])
}

fn default_reset() -> Weights<ResetKind> {
    Weights::from([(ResetKind::Watchdog, 1.0)])
}

/// The failure model's scalar parameters and signal and reset mixes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields, default)]
pub struct ModelDef {
    /// Failures per second one count past a limit.
    #[serde(skip_serializing_if = "skip::past_limit_rate")]
    pub past_limit_rate: f64,
    /// Factor on the rate for every further count past the limit; positive.
    #[serde(skip_serializing_if = "skip::growth")]
    pub growth: f64,
    /// Failures per second of a loaded core at or inside its limit.
    #[serde(skip_serializing_if = "skip::is_zero")]
    pub near_limit_rate: f64,
    /// Probability that a per-core crash leaves an MCE.
    #[serde(skip_serializing_if = "skip::crash_mce")]
    pub crash_mce: f64,
    /// Probability that an MCE bank is core-local rather than shared.
    #[serde(skip_serializing_if = "skip::core_local_bank")]
    pub core_local_bank: f64,
    /// Seconds at the start of a trial during which `onset_boost` applies.
    #[serde(skip_serializing_if = "skip::onset_s")]
    pub onset_s: f64,
    /// Extra relative rate during the onset window; rates are multiplied by `1 + onset_boost`.
    #[serde(skip_serializing_if = "skip::is_zero")]
    pub onset_boost: f64,
    #[serde(skip_serializing_if = "skip::signals")]
    pub signals: Weights<Signal>,
    /// Replaces `signals` for failures in each listed regime.
    #[serde(
        deserialize_with = "unique_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub regime_signals: BTreeMap<Regime, Weights<Signal>>,
    #[serde(skip_serializing_if = "skip::reset")]
    pub reset: Weights<ResetKind>,
}

impl ModelDef {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for ModelDef {
    fn default() -> Self {
        Self {
            past_limit_rate: DEFAULT_PAST_LIMIT_RATE,
            growth: DEFAULT_GROWTH,
            near_limit_rate: 0.0,
            crash_mce: DEFAULT_CRASH_MCE,
            core_local_bank: DEFAULT_CORE_LOCAL_BANK,
            onset_s: DEFAULT_ONSET_S,
            onset_boost: 0.0,
            signals: default_signals(),
            regime_signals: BTreeMap::new(),
            reset: default_reset(),
        }
    }
}

/// A smooth R7 hazard per loaded CCD from its mean applied depth.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields, default)]
pub struct CcdDef {
    pub log_rate: f64,
    /// Nonnegative.
    pub slope: f64,
    pub effect: [f64; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct CoreDef {
    pub id: i64,
    /// Deepest passing offsets of R1 to R5 with every other core at 0, each in -50..=1.
    pub alone: [i64; 5],
    /// Deepest passing offsets of R1 to R7 with other cores set, each in -50..=1.
    pub together: [i64; 7],
    /// Failures per second at any nonzero offset; nonnegative.
    #[serde(default, skip_serializing_if = "skip::is_zero")]
    pub flat: f64,
    /// Limit of the unloaded core; an unloaded core with no `idle` and no `flat` never fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<i64>,
    /// Limit per workload ID, overriding `alone` and `together`.
    #[serde(
        default,
        deserialize_with = "unique_map",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub workload: BTreeMap<String, i64>,
}

/// Cores that crash together once all are at or past their thresholds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields, default)]
pub struct JointDef {
    /// Core to threshold offset; the joint is active when every member is at or deeper than its threshold.
    #[serde(deserialize_with = "unique_map")]
    pub members: BTreeMap<i64, i64>,
    /// Regimes the joint applies to; all of them when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub regimes: Vec<Regime>,
    /// Failures per second while active; the model's `past_limit_rate` when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    /// Seconds into the trial before the joint can fail; nonnegative.
    #[serde(skip_serializing_if = "skip::is_zero")]
    pub after_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<Signal>,
    /// Core named by the uncorrected MCE each crash from this joint leaves in the next boot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crash_mce_core: Option<i64>,
}

/// Pins the outcome of one trial.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields, default)]
pub struct ScriptDef {
    pub trial: String,
    /// The signal to report; the trial passes when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<Signal>,
    /// Seconds into the trial; nonnegative, clamped to the trial's duration.
    #[serde(skip_serializing_if = "skip::is_zero")]
    pub at_s: f64,
    #[serde(skip_serializing_if = "skip::is_zero_core")]
    pub core: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset: Option<ResetKind>,
    /// Report `signal` and then crash.
    #[serde(skip_serializing_if = "skip::is_false")]
    pub then_crash: bool,
}

/// The common voltage rail of a 16-core machine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct SharedVoltageDef {
    pub idle_v: f64,
    /// Smoothing width of the hazard in volts.
    pub margin_v: f64,
    /// Failures per second per core at its threshold.
    pub rate: f64,
    /// A platform failure rate of every multi-core R7 trial, independent of the profile; nonnegative.
    #[serde(default, skip_serializing_if = "skip::is_zero")]
    pub background_rate: f64,
    pub power_limit_w: f64,
    pub thermal_limit_w: f64,
    /// Exactly the three R7 workload IDs.
    #[serde(deserialize_with = "unique_map")]
    pub workload: BTreeMap<String, VoltageWorkloadDef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct VoltageWorkloadDef {
    pub reference_mhz: f64,
    pub full_mhz: [f64; 2],
    #[serde(default, skip_serializing_if = "skip::is_zero")]
    pub idle_gain_mhz: f64,
    pub watts_per_core: f64,
    #[serde(default, skip_serializing_if = "skip::is_zero")]
    pub offset_watts_per_count: f64,
    #[serde(default, skip_serializing_if = "skip::is_zero")]
    pub package_mhz_per_w: f64,
    #[serde(default, skip_serializing_if = "skip::is_zero")]
    pub balance_mhz_per_w: f64,
    /// Sixteen entries in core-ID order.
    pub core: Vec<VoltageCoreDef>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct VoltageCoreDef {
    /// Requested voltage at offset 0 and the reference clock.
    pub base_v: f64,
    /// Required voltage at the reference clock.
    pub threshold_v: f64,
    /// Volts per count of offset; 0 selects [`DEFAULT_COUNT_V`].
    #[serde(default, skip_serializing_if = "skip::is_zero")]
    pub count_v: f64,
    #[serde(default, skip_serializing_if = "skip::is_zero")]
    pub clock_v_per_100mhz: f64,
    #[serde(default, skip_serializing_if = "skip::is_zero")]
    pub threshold_clock_v_per_100mhz: f64,
    /// Signal mix of this core's failures; the regime or pooled mix when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<Weights<Signal>>,
}

inherent_serialize!(
    MachineDefinition,
    BiosContextDef,
    ModelDef,
    CcdDef,
    CoreDef,
    JointDef,
    ScriptDef,
    SharedVoltageDef,
    VoltageWorkloadDef,
    VoltageCoreDef,
);
object!(
    MachineDefinition,
    BiosContextDef,
    ModelDef,
    CcdDef,
    CoreDef,
    JointDef,
    ScriptDef,
    SharedVoltageDef,
    VoltageWorkloadDef,
    VoltageCoreDef,
);
