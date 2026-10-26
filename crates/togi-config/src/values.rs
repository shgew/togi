use std::path::{Path, PathBuf};

use togi_machine::Backend;

use crate::error::{RangeError, RelativeBackendPath};

macro_rules! bounded {
    ($(#[$doc:meta])* $name:ident, $min:expr, $max:expr) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u32);

        impl $name {
            pub const MIN: u32 = $min;
            pub const MAX: u32 = $max;

            pub fn new(value: i64) -> Result<Self, RangeError> {
                u32::try_from(value)
                    .ok()
                    .filter(|seconds| (Self::MIN..=Self::MAX).contains(seconds))
                    .map(Self)
                    .ok_or(RangeError {
                        value,
                        min: Self::MIN,
                        max: Self::MAX,
                    })
            }

            pub(crate) const fn literal(value: u32) -> Self {
                assert!(value >= Self::MIN && value <= Self::MAX);
                Self(value)
            }

            pub const fn get(self) -> u32 {
                self.0
            }
        }

        impl TryFrom<i64> for $name {
            type Error = RangeError;
            fn try_from(value: i64) -> Result<Self, RangeError> {
                Self::new(value)
            }
        }
    };
}

bounded!(
    /// How many consecutive occurrences of a dead-end signal end the session.
    DeadEndThreshold, 1, 100
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadEndPolicy {
    inconclusive_in_a_row: DeadEndThreshold,
    stray_crashes_in_a_row: DeadEndThreshold,
}

impl DeadEndPolicy {
    pub const fn new(
        inconclusive_in_a_row: DeadEndThreshold,
        stray_crashes_in_a_row: DeadEndThreshold,
    ) -> Self {
        Self {
            inconclusive_in_a_row,
            stray_crashes_in_a_row,
        }
    }

    pub const fn inconclusive_in_a_row(self) -> DeadEndThreshold {
        self.inconclusive_in_a_row
    }

    pub const fn stray_crashes_in_a_row(self) -> DeadEndThreshold {
        self.stray_crashes_in_a_row
    }
}

impl Default for DeadEndPolicy {
    fn default() -> Self {
        Self::new(DeadEndThreshold::literal(3), DeadEndThreshold::literal(3))
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Backends {
    mprime: Option<PathBuf>,
    ycruncher: Option<PathBuf>,
}

impl Backends {
    pub fn new(
        mprime: Option<PathBuf>,
        ycruncher: Option<PathBuf>,
    ) -> Result<Self, RelativeBackendPath> {
        for (backend, path) in [(Backend::Mprime, &mprime), (Backend::Ycruncher, &ycruncher)] {
            if let Some(path) = path.as_ref().filter(|path| !path.is_absolute()) {
                return Err(RelativeBackendPath {
                    backend,
                    path: path.clone(),
                });
            }
        }
        Ok(Self { mprime, ycruncher })
    }

    pub fn mprime(&self) -> Option<&Path> {
        self.mprime.as_deref()
    }

    pub fn ycruncher(&self) -> Option<&Path> {
        self.ycruncher.as_deref()
    }

    pub fn path(&self, backend: Backend) -> Option<&Path> {
        match backend {
            Backend::Mprime => self.mprime(),
            Backend::Ycruncher => self.ycruncher(),
        }
    }
}
