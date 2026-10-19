use crate::{CoreId, PER_CORE_MAX};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

/// Bounded per-core storage, iterated and serialized in ascending core order.
///
/// Only cores written with [`Self::set`] are present. Sparse state uses
/// `PerCore<Option<T>>`: setting a present core to `None` clears its state but
/// retains its presence, including a `null` entry when serialized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerCore<T> {
    values: [T; PER_CORE_MAX],
    present: u16,
}

impl<T: Default> Default for PerCore<T> {
    fn default() -> Self {
        Self {
            values: std::array::from_fn(|_| T::default()),
            present: 0,
        }
    }
}

impl<T> PerCore<T> {
    pub fn set(&mut self, core: CoreId, value: T) {
        self.values[core.index()] = value;
        self.present |= 1 << core.get();
    }

    pub fn get_ref(&self, core: CoreId) -> Option<&T> {
        (self.present & (1 << core.get()) != 0).then(|| &self.values[core.index()])
    }

    /// Borrows a present core's value without changing topology presence.
    pub fn get_mut(&mut self, core: CoreId) -> Option<&mut T> {
        (self.present & (1 << core.get()) != 0).then(|| &mut self.values[core.index()])
    }

    pub fn iter_ref(&self) -> impl Iterator<Item = (CoreId, &T)> {
        (0..16_u8).filter_map(|index| {
            let core = CoreId::from_index(index);
            self.get_ref(core).map(|value| (core, value))
        })
    }

    /// Counts present cores, including those holding `None` in sparse storage.
    pub const fn len(&self) -> usize {
        self.present.count_ones() as usize
    }
    pub const fn is_empty(&self) -> bool {
        self.present == 0
    }
}

impl<T: Copy> PerCore<T> {
    pub fn get(&self, core: CoreId) -> Option<T> {
        self.get_ref(core).copied()
    }
    pub fn iter(&self) -> impl Iterator<Item = (CoreId, T)> + '_ {
        (0..16_u8).filter_map(|index| {
            let core = CoreId::from_index(index);
            self.get(core).map(|value| (core, value))
        })
    }
}

impl<T: Serialize> Serialize for PerCore<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter_ref())
    }
}

impl<'de, T: Default + Deserialize<'de>> Deserialize<'de> for PerCore<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(BTreeMap::<CoreId, T>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

impl<T: Default> FromIterator<(CoreId, T)> for PerCore<T> {
    fn from_iter<I: IntoIterator<Item = (CoreId, T)>>(readings: I) -> Self {
        let mut result = Self::default();
        for (core, value) in readings {
            result.set(core, value);
        }
        result
    }
}

impl<T> PerCore<T> {
    /// Moves present values out in ascending core order without cloning them.
    pub fn into_iter_values(self) -> impl Iterator<Item = (CoreId, T)> {
        let Self { values, present } = self;
        (0_u8..).zip(values).filter_map(move |(index, value)| {
            let core = CoreId::from_index(index);
            (present & (1 << core.get()) != 0).then_some((core, value))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_sparse_values_preserve_holes_order_and_wire_shape() {
        let low = CoreId::from_index(2);
        let high = CoreId::from_index(15);
        let hole = CoreId::from_index(7);
        let mut values: PerCore<Option<Vec<u32>>> = [(high, Some(vec![15])), (low, Some(vec![2]))]
            .into_iter()
            .collect();
        assert_eq!(values.get_ref(hole), None);
        values.get_mut(low).unwrap().as_mut().unwrap().push(3);
        assert_eq!(
            values.iter_ref().map(|(core, _)| core).collect::<Vec<_>>(),
            vec![low, high]
        );
        let encoded = serde_json::to_string(&values).unwrap();
        assert_eq!(encoded, r#"{"2":[2,3],"15":[15]}"#);
        assert_eq!(
            serde_json::from_str::<PerCore<Option<Vec<u32>>>>(&encoded).unwrap(),
            values
        );
        assert_eq!(values.get_mut(high).and_then(Option::take), Some(vec![15]));
        assert_eq!(values.get_ref(high), Some(&None));
        assert_eq!(values.get_ref(hole), None);
        assert_eq!(values.len(), 2);
        assert_eq!(
            serde_json::to_string(&values).unwrap(),
            r#"{"2":[2,3],"15":null}"#
        );
        assert_eq!(
            values.into_iter_values().collect::<Vec<_>>(),
            vec![(low, Some(vec![2, 3])), (high, None)]
        );
    }

    #[test]
    fn sparse_values_do_not_need_a_default_payload() {
        #[derive(Debug, Eq, PartialEq)]
        struct Owned(String);

        let core = CoreId::from_index(15);
        let mut values = PerCore::<Option<Owned>>::default();
        assert!(values.is_empty());
        assert_eq!(values.get_mut(core), None);
        values.set(core, Some(Owned("first".into())));
        values.set(core, Some(Owned("replacement".into())));
        assert_eq!(values.len(), 1);
        assert_eq!(
            values.into_iter_values().collect::<Vec<_>>(),
            vec![(core, Some(Owned("replacement".into())))]
        );
    }
}
