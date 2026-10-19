use std::fmt;

/// A machine definition that [`Model::new`](crate::Model::new) refuses, with the field it refuses.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{path}: {problem}")]
pub struct InvalidMachine {
    /// The field path as written in the machine file, such as `joint[2].members.3`.
    pub path: String,
    pub problem: Problem,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Problem {
    #[error("must be finite")]
    NotFinite,
    #[error("must be positive")]
    NotPositive,
    #[error("must not be negative")]
    Negative,
    #[error("must be between 0 and 1")]
    Probability,
    #[error("{value} is outside {min}..={max}")]
    OutOfRange { value: i64, min: i64, max: i64 },
    #[error("{found} values for {expected} cores")]
    Length { expected: usize, found: usize },
    #[error("{0} cores: must be even, from 2 to 16")]
    Topology(i64),
    #[error("duplicate")]
    Duplicate,
    #[error("core {0} is missing")]
    MissingCore(usize),
    #[error("weights must sum to a positive total")]
    ZeroTotal,
    #[error("must not be empty")]
    Empty,
    #[error("rates overflow within the offset range")]
    Overflow,
    #[error("needs exactly 16 cores")]
    NeedsSixteenCores,
    #[error("is not a failure signal a simulated machine can draw")]
    Unsupported,
    #[error("must name exactly the three R7 workloads")]
    Workloads,
}

#[derive(Clone, Copy, Debug)]
enum Segment<'a> {
    Key(&'a str),
    Index(usize),
    Number(i64),
}

/// A field path built on the stack while validating and rendered only for an error.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Path<'a> {
    parent: Option<&'a Path<'a>>,
    segment: Segment<'a>,
}

impl<'a> Path<'a> {
    pub(crate) const fn root(key: &'a str) -> Self {
        Self {
            parent: None,
            segment: Segment::Key(key),
        }
    }

    pub(crate) const fn key(&'a self, key: &'a str) -> Self {
        Self {
            parent: Some(self),
            segment: Segment::Key(key),
        }
    }

    pub(crate) const fn index(&'a self, index: usize) -> Self {
        Self {
            parent: Some(self),
            segment: Segment::Index(index),
        }
    }

    pub(crate) const fn number(&'a self, number: i64) -> Self {
        Self {
            parent: Some(self),
            segment: Segment::Number(number),
        }
    }

    pub(crate) fn error(&self, problem: Problem) -> InvalidMachine {
        InvalidMachine {
            path: self.to_string(),
            problem,
        }
    }

    pub(crate) fn finite(&self, value: f64) -> Result<f64, InvalidMachine> {
        if value.is_finite() {
            Ok(value)
        } else {
            Err(self.error(Problem::NotFinite))
        }
    }

    pub(crate) fn positive(&self, value: f64) -> Result<f64, InvalidMachine> {
        self.finite(value).and_then(|value| {
            if value > 0.0 {
                Ok(value)
            } else {
                Err(self.error(Problem::NotPositive))
            }
        })
    }

    pub(crate) fn nonnegative(&self, value: f64) -> Result<f64, InvalidMachine> {
        self.finite(value).and_then(|value| {
            if value >= 0.0 {
                Ok(value)
            } else {
                Err(self.error(Problem::Negative))
            }
        })
    }

    pub(crate) fn probability(&self, value: f64) -> Result<f64, InvalidMachine> {
        self.finite(value).and_then(|value| {
            if (0.0..=1.0).contains(&value) {
                Ok(value)
            } else {
                Err(self.error(Problem::Probability))
            }
        })
    }

    pub(crate) fn within(&self, value: i64, min: i64, max: i64) -> Result<i64, InvalidMachine> {
        if (min..=max).contains(&value) {
            Ok(value)
        } else {
            Err(self.error(Problem::OutOfRange { value, min, max }))
        }
    }

    pub(crate) fn length(&self, expected: usize, found: usize) -> Result<(), InvalidMachine> {
        if expected == found {
            Ok(())
        } else {
            Err(self.error(Problem::Length { expected, found }))
        }
    }
}

impl fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(parent) = self.parent {
            parent.fmt(f)?;
        }
        match (self.parent, self.segment) {
            (None, Segment::Key(key)) => f.write_str(key),
            (Some(_), Segment::Key(key)) => write!(f, ".{key}"),
            (_, Segment::Index(index)) => write!(f, "[{index}]"),
            (None, Segment::Number(number)) => write!(f, "{number}"),
            (Some(_), Segment::Number(number)) => write!(f, ".{number}"),
        }
    }
}

/// A profile or loaded set that does not fit the model it is evaluated on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ShapeError {
    #[error("profile has {found} offsets for {expected} cores")]
    Profile { expected: usize, found: usize },
    #[error("loaded core {core} does not exist on a {cores}-core machine")]
    LoadedCore { core: u8, cores: usize },
}
