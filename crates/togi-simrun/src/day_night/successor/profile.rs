//! Preserve exact physical-core vectors without accepting duplicate JSON object keys.

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use togi_machine::{CoreId, PerCore};

struct StrictProfile(PerCore<u8>);

/// Tagged enum buffering preserves JSON map keys as strings rather than map-key
/// deserializers. Decode the same integer key in either representation without allocation.
struct CoreKey(CoreId);

impl<'de> Deserialize<'de> for CoreKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Key;
        impl Visitor<'_> for Key {
            type Value = CoreKey;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a physical-core integer object key")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<CoreKey, E> {
                let index = value.parse::<i64>().map_err(E::custom)?;
                CoreId::new(index).map(CoreKey).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Key)
    }
}

impl<'de> Deserialize<'de> for StrictProfile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Cores;
        impl<'de> Visitor<'de> for Cores {
            type Value = StrictProfile;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object of unique physical-core depths in 0..=50")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut profile = PerCore::default();
                while let Some((CoreKey(core), depth)) = map.next_entry::<CoreKey, u8>()? {
                    if depth > 50 || profile.get(core).is_some() {
                        return Err(serde::de::Error::custom(
                            "invalid or duplicate physical-core depth",
                        ));
                    }
                    profile.set(core, depth);
                }
                Ok(StrictProfile(profile))
            }
        }
        deserializer.deserialize_map(Cores)
    }
}

pub(super) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<PerCore<u8>, D::Error> {
    StrictProfile::deserialize(deserializer).map(|profile| profile.0)
}

pub(super) fn deserialize_optional<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PerCore<u8>>, D::Error> {
    Option::<StrictProfile>::deserialize(deserializer)
        .map(|profile| profile.map(|profile| profile.0))
}
