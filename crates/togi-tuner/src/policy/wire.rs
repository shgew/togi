//! Serde helpers for the fold's books.

/// A map as a sequence of pairs, so keys that are not strings survive JSON.
pub(super) mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(in crate::policy) fn serialize<'a, M, K, V, S>(
        map: &'a M,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        &'a M: IntoIterator<Item = (&'a K, &'a V)>,
        K: Serialize + 'a,
        V: Serialize + 'a,
        S: Serializer,
    {
        serializer.collect_seq(map)
    }

    pub(in crate::policy) fn deserialize<'de, M, K, V, D>(deserializer: D) -> Result<M, D::Error>
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

/// A hashed map of hashed sets as [`togi_machine::serde_pairs`] writes it, each set in the stable order too, so a
/// decoded copy re-encodes to the same JSON whatever its hash seed.
pub(super) mod pairs_of_sets {
    use std::collections::{HashMap, HashSet};
    use std::hash::{BuildHasher, Hash};

    use serde::{Serialize, Serializer};

    pub(in crate::policy) use togi_machine::serde_pairs::deserialize;

    pub(in crate::policy) fn serialize<K, T, H, I, S>(
        map: &HashMap<K, HashSet<T, I>, H>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        K: Eq + Hash + Serialize,
        T: Serialize,
        H: BuildHasher,
        S: Serializer,
    {
        let sets: HashMap<&K, StableSet<'_, T, I>> =
            map.iter().map(|(key, set)| (key, StableSet(set))).collect();
        togi_machine::serde_pairs::serialize(&sets, serializer)
    }

    struct StableSet<'a, T, I>(&'a HashSet<T, I>);

    impl<T: Serialize, I> Serialize for StableSet<'_, T, I> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            togi_machine::serde_pairs::serialize_set(self.0, serializer)
        }
    }
}
