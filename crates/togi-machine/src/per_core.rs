use crate::{CoreId, PER_CORE_MAX};
use serde::de::{DeserializeOwned, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use std::fmt;

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
    pub fn from_readings(readings: impl IntoIterator<Item = (CoreId, T)>) -> Self {
        let mut result = Self::default();
        for (core, value) in readings {
            result.set(core, value);
        }
        result
    }

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

    pub fn all(&self) -> impl Iterator<Item = (CoreId, T)> + '_ {
        (0..16_u8).filter_map(|index| {
            let core = CoreId::from_index(index);
            self.get(core).map(|value| (core, value))
        })
    }
}

const STRING_ORDER: [(u8, &str); 16] = [
    (0, "0"),
    (1, "1"),
    (10, "10"),
    (11, "11"),
    (12, "12"),
    (13, "13"),
    (14, "14"),
    (15, "15"),
    (2, "2"),
    (3, "3"),
    (4, "4"),
    (5, "5"),
    (6, "6"),
    (7, "7"),
    (8, "8"),
    (9, "9"),
];

impl<T: Copy + Default + Serialize> Serialize for PerCore<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (index, key) in STRING_ORDER {
            if let Some(value) = self.get(CoreId::from_index(index)) {
                map.serialize_entry(key, &value)?;
            }
        }
        map.end()
    }
}

impl<'de, T: Copy + Default + DeserializeOwned> Deserialize<'de> for PerCore<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Readings<T>(std::marker::PhantomData<T>);
        impl<'de, T: Copy + Default + DeserializeOwned> Visitor<'de> for Readings<T> {
            type Value = PerCore<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a core-keyed object or null")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(PerCore::default())
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut readings = PerCore::default();
                while let Some((key, raw)) = map.next_entry::<String, Box<RawValue>>()? {
                    let core = CoreId::new(key.parse::<i64>().map_err(serde::de::Error::custom)?)
                        .map_err(serde::de::Error::custom)?;
                    let value = if raw.get() == "null" {
                        T::default()
                    } else {
                        serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?
                    };
                    readings.set(core, value);
                }
                Ok(readings)
            }
        }
        deserializer.deserialize_any(Readings(std::marker::PhantomData))
    }
}
