use togi_machine::{CoreId, PER_CORE_MAX};

/// A set of cores, ordered and without duplicates by construction.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct CoreSet(u16);

impl CoreSet {
    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn insert(&mut self, core: CoreId) {
        self.0 |= 1 << core.get();
    }

    pub const fn contains(self, core: CoreId) -> bool {
        self.0 & (1 << core.get()) != 0
    }

    pub const fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The highest core in the set, or `None` when it is empty.
    pub fn highest(self) -> Option<CoreId> {
        core_id(usize::try_from(self.0.checked_ilog2()?).ok()?)
    }

    /// Cores in ascending order.
    pub fn iter(self) -> impl Iterator<Item = CoreId> {
        ids(PER_CORE_MAX).filter(move |&core| self.contains(core))
    }
}

impl FromIterator<CoreId> for CoreSet {
    fn from_iter<I: IntoIterator<Item = CoreId>>(cores: I) -> Self {
        let mut set = Self::new();
        for core in cores {
            set.insert(core);
        }
        set
    }
}

pub(crate) fn core_id(index: usize) -> Option<CoreId> {
    CoreId::new(i64::try_from(index).ok()?).ok()
}

/// The first `count` core IDs in ascending order.
pub(crate) fn ids(count: usize) -> impl Iterator<Item = CoreId> {
    (0..count).filter_map(core_id)
}
