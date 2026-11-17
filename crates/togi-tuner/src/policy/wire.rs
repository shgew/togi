//! Serde helpers for the fold's books.

/// A map as a sequence of pairs, so keys that are not strings survive JSON.
pub(crate) mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<'a, M, K, V, S>(map: &'a M, serializer: S) -> Result<S::Ok, S::Error>
    where
        &'a M: IntoIterator<Item = (&'a K, &'a V)>,
        K: Serialize + 'a,
        V: Serialize + 'a,
        S: Serializer,
    {
        serializer.collect_seq(map)
    }

    pub(crate) fn deserialize<'de, M, K, V, D>(deserializer: D) -> Result<M, D::Error>
    where
        M: FromIterator<(K, V)>,
        K: Deserialize<'de>,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}
