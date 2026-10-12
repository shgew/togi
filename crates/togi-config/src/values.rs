use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use togi_machine::{Backend, Regime};

use crate::error::{EmptySchedule, ProbabilityError, RangeError, RelativeBackendPath, TrialsError};

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
    /// Whole seconds of one trial or idle period; the length is part of a trial's class.
    TrialSeconds, 1, 86_400
);
bounded!(
    /// Whole seconds of a long R7 trial, at least four so that a quarter per CCD is not zero.
    R7BudgetSeconds, 4, 86_400
);
bounded!(
    /// How many consecutive occurrences of a dead-end signal end the session.
    DeadEndThreshold, 1, 100
);

impl TrialSeconds {
    pub const fn as_duration(self) -> Duration {
        Duration::from_secs(self.0 as u64)
    }
}

impl R7BudgetSeconds {
    pub const fn as_duration(self) -> Duration {
        Duration::from_secs(self.0 as u64)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Probability(f64);

impl Probability {
    pub fn new(value: f64) -> Result<Self, ProbabilityError> {
        if value > 0.0 && value < 1.0 {
            Ok(Self(value))
        } else {
            Err(ProbabilityError(value))
        }
    }

    pub(crate) const fn literal(value: f64) -> Self {
        assert!(value > 0.0 && value < 1.0);
        Self(value)
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Probability {
    type Error = ProbabilityError;
    fn try_from(value: f64) -> Result<Self, ProbabilityError> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TrialCount(u16);

impl TrialCount {
    pub const MAX: u16 = 1000;

    pub const fn get(self) -> u32 {
        self.0 as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EvidencePolicy {
    miss: Probability,
    rate: Probability,
    trials: TrialCount,
}

impl EvidencePolicy {
    pub fn new(miss: Probability, rate: Probability) -> Result<Self, TrialsError> {
        let needed = (miss.0.ln() / (-rate.0).ln_1p()).ceil();
        if !(1.0..=f64::from(TrialCount::MAX)).contains(&needed) {
            return Err(TrialsError {
                miss: miss.0,
                rate: rate.0,
            });
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "needed is an integer within 1..=1000 here"
        )]
        let trials = TrialCount(needed as u16);
        Ok(Self { miss, rate, trials })
    }

    pub const fn miss(self) -> Probability {
        self.miss
    }

    pub const fn rate(self) -> Probability {
        self.rate
    }

    pub const fn trials(self) -> TrialCount {
        self.trials
    }
}

impl Default for EvidencePolicy {
    fn default() -> Self {
        Self {
            miss: Probability::literal(0.05),
            rate: Probability::literal(0.5),
            trials: TrialCount(5),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckingSchedule(Vec<Regime>);

impl CheckingSchedule {
    pub fn new(regimes: Vec<Regime>) -> Result<Self, EmptySchedule> {
        if regimes.is_empty() {
            Err(EmptySchedule)
        } else {
            Ok(Self(regimes))
        }
    }

    pub fn as_slice(&self) -> &[Regime] {
        &self.0
    }

    pub fn iter(&self) -> std::iter::Copied<std::slice::Iter<'_, Regime>> {
        self.0.iter().copied()
    }

    pub fn is_full(&self) -> bool {
        FULL_CYCLE
            .iter()
            .all(|&(regime, least)| self.iter().filter(|&step| step == regime).count() >= least)
    }
}

/// The steps of each regime a schedule needs for its passed cycle to count as full.
pub const FULL_CYCLE: [(Regime, usize); 7] = [
    (Regime::R1, 3),
    (Regime::R2, 3),
    (Regime::R3, 1),
    (Regime::R4, 1),
    (Regime::R5, 1),
    (Regime::R6, 1),
    (Regime::R7, 3),
];

impl Default for CheckingSchedule {
    fn default() -> Self {
        use Regime::{R1, R2, R3, R4, R5, R6, R7};
        Self(vec![
            R7, R7, R7, R2, R2, R2, R6, R5, R1, R1, R1, R3, R3, R3, R4, R4, R4, R6,
        ])
    }
}

impl<'a> IntoIterator for &'a CheckingSchedule {
    type Item = Regime;
    type IntoIter = std::iter::Copied<std::slice::Iter<'a, Regime>>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

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
