use crate::CoreId;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A set of cores, iterated in ascending order and serialized as a sorted array of core ids.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct CoreSet {
    present: u16,
}

impl CoreSet {
    /// Returns whether `core` was newly added.
    pub fn insert(&mut self, core: CoreId) -> bool {
        let added = !self.contains(core);
        self.present |= 1 << core.get();
        added
    }

    /// Returns whether `core` was present.
    pub fn remove(&mut self, core: CoreId) -> bool {
        let removed = self.contains(core);
        self.present &= !(1 << core.get());
        removed
    }

    pub const fn contains(&self, core: CoreId) -> bool {
        self.present & (1 << core.get()) != 0
    }

    pub const fn len(&self) -> usize {
        self.present.count_ones() as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.present == 0
    }

    /// The lowest core in the set.
    pub fn first(&self) -> Option<CoreId> {
        self.iter().next()
    }

    /// The highest core in the set.
    pub fn last(&self) -> Option<CoreId> {
        self.iter().next_back()
    }

    pub const fn iter(&self) -> CoreSetIter {
        CoreSetIter {
            remaining: self.present,
        }
    }
}

/// Ascending iterator over a [`CoreSet`].
#[derive(Clone, Debug)]
pub struct CoreSetIter {
    remaining: u16,
}

impl Iterator for CoreSetIter {
    type Item = CoreId;

    fn next(&mut self) -> Option<CoreId> {
        if self.remaining == 0 {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a nonempty u16 mask has bit indices 0..16"
        )]
        let index = self.remaining.trailing_zeros() as u8;
        self.remaining &= self.remaining - 1;
        Some(CoreId::from_index(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.remaining.count_ones() as usize;
        (left, Some(left))
    }
}

impl DoubleEndedIterator for CoreSetIter {
    fn next_back(&mut self) -> Option<CoreId> {
        if self.remaining == 0 {
            return None;
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a nonempty u16 mask has bit indices 0..16"
        )]
        let index = self.remaining.ilog2() as u8;
        self.remaining &= !(1 << index);
        Some(CoreId::from_index(index))
    }
}

impl ExactSizeIterator for CoreSetIter {}

impl FromIterator<CoreId> for CoreSet {
    fn from_iter<I: IntoIterator<Item = CoreId>>(cores: I) -> Self {
        let mut set = Self::default();
        set.extend(cores);
        set
    }
}

impl Extend<CoreId> for CoreSet {
    fn extend<I: IntoIterator<Item = CoreId>>(&mut self, cores: I) {
        for core in cores {
            self.insert(core);
        }
    }
}

impl IntoIterator for CoreSet {
    type Item = CoreId;
    type IntoIter = CoreSetIter;

    fn into_iter(self) -> CoreSetIter {
        self.iter()
    }
}

impl IntoIterator for &CoreSet {
    type Item = CoreId;
    type IntoIter = CoreSetIter;

    fn into_iter(self) -> CoreSetIter {
        self.iter()
    }
}

impl Serialize for CoreSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

impl<'de> Deserialize<'de> for CoreSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut set = Self::default();
        for core in Vec::<CoreId>::deserialize(deserializer)? {
            if !set.insert(core) {
                return Err(D::Error::custom(format!("core {core} listed twice")));
            }
        }
        Ok(set)
    }
}
