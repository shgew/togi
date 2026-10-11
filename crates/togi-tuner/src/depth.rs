use togi_machine::TargetOffset;

/// Compares and steps offsets by depth: a deeper offset is further below zero.
pub trait Depth: Sized {
    fn deeper_than(self, other: Self) -> bool;
    fn shallower_than(self, other: Self) -> bool;
    /// Steps `counts` toward zero, stopping at zero.
    #[must_use]
    fn shallower_by(self, counts: u8) -> Self;
    /// Steps `counts` away from zero, or `None` past the floor.
    fn deeper_by(self, counts: u8) -> Option<Self>;
    /// The offset as a signed count for summing depths; the type bounds it to -50 through 0.
    fn signed(self) -> i32;
}

impl Depth for TargetOffset {
    fn deeper_than(self, other: Self) -> bool {
        self.get() < other.get()
    }

    fn shallower_than(self, other: Self) -> bool {
        self.get() > other.get()
    }

    fn shallower_by(self, counts: u8) -> Self {
        Self::clamped(self.get() + i64::from(counts))
    }

    fn deeper_by(self, counts: u8) -> Option<Self> {
        Self::new(self.get() - i64::from(counts)).ok()
    }

    fn signed(self) -> i32 {
        i32::try_from(self.get()).unwrap_or(i32::MIN)
    }
}
