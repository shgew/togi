use std::num::NonZeroU64;

/// The position of an event in the journal; later events have greater seqs.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Seq(NonZeroU64);

impl Seq {
    pub const fn new(seq: NonZeroU64) -> Self {
        Self(seq)
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}
