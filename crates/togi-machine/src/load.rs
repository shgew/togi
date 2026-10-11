use crate::{Regime, TrialSpec};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

const RANDOM_PERIODS: [Duration; 5] = [
    Duration::from_millis(10),
    Duration::from_millis(50),
    Duration::from_millis(200),
    Duration::from_secs(1),
    Duration::from_secs(5),
];

const DUTY_PERIOD: Duration = Duration::from_millis(100);

/// A duty cycle as a whole percentage from 0 to 100.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct DutyPct(u8);

impl DutyPct {
    pub const fn new(pct: u8) -> Option<Self> {
        if pct <= 100 { Some(Self(pct)) } else { None }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

/// A duty percentage above 100.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("duty percentage {0} is above 100")]
pub struct InvalidDutyPct(u8);

impl TryFrom<u8> for DutyPct {
    type Error = InvalidDutyPct;

    fn try_from(pct: u8) -> Result<Self, InvalidDutyPct> {
        Self::new(pct).ok_or(InvalidDutyPct(pct))
    }
}

impl From<DutyPct> for u8 {
    fn from(pct: DutyPct) -> Self {
        pct.0
    }
}

impl fmt::Display for DutyPct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// How a load-step trial alternates its workload between running and stopped.
///
/// Each on period ends with `SIGSTOP` and each off period with `SIGCONT`. Random periods come from
/// `rand::rngs::StdRng` seeded with the trial seed. A recorded seed reproduces its stream with the
/// build that recorded it, since `rand` is pinned to an exact version and the journal's build stamp
/// identifies it; a `rand` bump changes the stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoadSchedule {
    Random { seed: u64 },
    Duty { pct: DutyPct },
}

/// How many stop and continue signals a schedule has sent.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Toggles {
    pub stops: u32,
    pub conts: u32,
}

impl LoadSchedule {
    pub fn for_spec(spec: &TrialSpec) -> Option<Self> {
        match spec.regime {
            Regime::R3 => Some(Self::Random { seed: spec.seed }),
            Regime::R4 => Some(Self::Duty {
                pct: spec.workload.duty_pct,
            }),
            Regime::R1 | Regime::R2 | Regime::R5 | Regime::R6 | Regime::R7 => None,
        }
    }

    /// Yields on, off, on, off, … forever.
    pub fn periods(&self) -> impl Iterator<Item = Duration> + use<> {
        match *self {
            Self::Duty { pct } => {
                let on = DUTY_PERIOD * u32::from(pct.get()) / 100;
                Periods::Duty {
                    on,
                    off: DUTY_PERIOD.saturating_sub(on),
                    next_is_on: true,
                }
            }
            Self::Random { seed } => Periods::Random(Box::new(StdRng::seed_from_u64(seed))),
        }
    }

    /// Counts the periods that ended before `ran`; a period ending at `ran` is not counted.
    pub fn toggles(&self, ran: Duration) -> Toggles {
        let mut toggles = Toggles::default();
        let mut elapsed = Duration::ZERO;
        for (index, period) in self.periods().enumerate() {
            elapsed += period;
            if elapsed >= ran {
                break;
            }
            if index % 2 == 0 {
                toggles.stops += 1;
            } else {
                toggles.conts += 1;
            }
        }
        toggles
    }
}

enum Periods {
    Duty {
        on: Duration,
        off: Duration,
        next_is_on: bool,
    },
    Random(Box<StdRng>),
}

impl Iterator for Periods {
    type Item = Duration;

    fn next(&mut self) -> Option<Duration> {
        Some(match self {
            Self::Duty {
                on,
                off,
                next_is_on,
            } => {
                let period = if *next_is_on { *on } else { *off };
                *next_is_on = !*next_is_on;
                period
            }
            Self::Random(rng) => RANDOM_PERIODS[rng.random_range(0..RANDOM_PERIODS.len())],
        })
    }
}

impl fmt::Display for LoadSchedule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duty { pct } => {
                write!(f, "{pct}% duty, ")?;
                write_period(f, DUTY_PERIOD)?;
                f.write_str(" period")
            }
            Self::Random { .. } => {
                f.write_str("random on/off periods from ")?;
                for (index, period) in RANDOM_PERIODS.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write_period(f, *period)?;
                }
                Ok(())
            }
        }
    }
}

fn write_period(f: &mut fmt::Formatter<'_>, period: Duration) -> fmt::Result {
    if period.subsec_millis() == 0 && !period.is_zero() {
        write!(f, "{} s", period.as_secs())
    } else {
        write!(f, "{} ms", period.as_millis())
    }
}
