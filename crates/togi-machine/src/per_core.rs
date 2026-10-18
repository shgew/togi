use crate::{CoreId, PER_CORE_MAX};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

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

    pub fn get_mut(&mut self, core: CoreId) -> Option<&mut T> {
        (self.present & (1 << core.get()) != 0).then(|| &mut self.values[core.index()])
    }

    pub fn iter_ref(&self) -> impl Iterator<Item = (CoreId, &T)> {
        (0..16_u8).filter_map(|index| {
            let core = CoreId::from_index(index);
            self.get_ref(core).map(|value| (core, value))
        })
    }
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
    }
}
