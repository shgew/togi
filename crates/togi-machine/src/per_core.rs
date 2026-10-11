use crate::{CoreId, PER_CORE_MAX};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerCore<T> {
    values: [T; PER_CORE_MAX],
    present: u16,
}

impl<T: Copy + Default> Default for PerCore<T> {
    fn default() -> Self {
        Self {
            values: [T::default(); PER_CORE_MAX],
            present: 0,
        }
    }
}

impl<T: Copy + Default> PerCore<T> {
    pub fn set(&mut self, core: CoreId, value: T) {
        self.values[core.index()] = value;
        self.present |= 1 << core.get();
    }

    pub fn get(&self, core: CoreId) -> Option<T> {
        (self.present & (1 << core.get()) != 0).then(|| self.values[core.index()])
    }

    pub const fn len(&self) -> usize {
        self.present.count_ones() as usize
    }
    pub const fn is_empty(&self) -> bool {
        self.present == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = (CoreId, T)> + '_ {
        (0..16_u8).filter_map(|index| {
            let core = CoreId::from_index(index);
            self.get(core).map(|value| (core, value))
        })
    }
}

impl<T: Copy + Default + Serialize> Serialize for PerCore<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

impl<'de, T: Copy + Default + Deserialize<'de>> Deserialize<'de> for PerCore<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(BTreeMap::<CoreId, T>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

impl<T: Copy + Default> FromIterator<(CoreId, T)> for PerCore<T> {
    fn from_iter<I: IntoIterator<Item = (CoreId, T)>>(readings: I) -> Self {
        let mut result = Self::default();
        for (core, value) in readings {
            result.set(core, value);
        }
        result
    }
}
