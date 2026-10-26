use std::num::NonZeroU32;

/// A trial's intended duration in whole seconds.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct Secs(NonZeroU32);

impl Secs {
    pub const fn new(secs: NonZeroU32) -> Self {
        Self(secs)
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}
