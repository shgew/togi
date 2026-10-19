//! JSON entry sequences for reducer maps whose keys are structured values.

use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serializes a map without requiring its keys to be JSON object member names.
///
/// # Errors
///
/// Returns the serializer's error.
pub fn serialize<'a, S, K: 'a + Serialize, V: 'a + Serialize, C>(
    map: &'a C,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    &'a C: IntoIterator<Item = (&'a K, &'a V)>,
{
    let mut entries = map
        .into_iter()
        .map(|pair| {
            serde_json::to_vec(pair.0)
                .map(|key| (key, pair))
                .map_err(serde::ser::Error::custom)
        })
        .collect::<Result<Vec<_>, S::Error>>()?;
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut sequence = serializer.serialize_seq(Some(entries.len()))?;
    for (_, pair) in entries {
        sequence.serialize_element(&pair)?;
    }
    sequence.end()
}

/// Restores a map from its entry sequence.
///
/// # Errors
///
/// Returns the deserializer's error.
pub fn deserialize<'de, D, K, V, C>(deserializer: D) -> Result<C, D::Error>
where
    D: Deserializer<'de>,
    K: Deserialize<'de>,
    V: Deserialize<'de>,
    C: FromIterator<(K, V)>,
{
    Vec::<(K, V)>::deserialize(deserializer)
        .map(IntoIterator::into_iter)
        .map(Iterator::collect)
}

/// Serializes a reducer set in a stable order, independent of its hash seed.
///
/// # Errors
///
/// Returns the serializer's error.
pub fn serialize_set<'a, S, T: 'a + Serialize, C>(
    set: &'a C,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    &'a C: IntoIterator<Item = &'a T>,
{
    let mut entries = set
        .into_iter()
        .map(|value| {
            serde_json::to_vec(value)
                .map(|key| (key, value))
                .map_err(serde::ser::Error::custom)
        })
        .collect::<Result<Vec<_>, S::Error>>()?;
    entries.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    let mut sequence = serializer.serialize_seq(Some(entries.len()))?;
    for (_, value) in entries {
        sequence.serialize_element(value)?;
    }
    sequence.end()
}
