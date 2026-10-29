//! The registered numerical rules and per-scenario applicability the scorer evaluates against.

use serde::Serialize;

use super::super::registration::{Criteria, Registration, Scenario};

/// Largest single promotion step in depth counts: the settled `min(5, ceil(distance / 2))` rule of
/// the successor policy. It is a policy rule, not an acceptance threshold, so the registration does
/// not carry it.
pub const MAX_STEP: u8 = 5;

/// Registered values the scorer reads; the registration locks them, the scorer never relaxes one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Rules {
    pub criteria: Criteria,
    /// λ*: CO-added crashes per active machine-hour.
    pub lambda_per_hour: f64,
    /// Absolute computational tolerance on hazard ratios against 1.
    pub ratio_tolerance: f64,
    /// Absolute computational tolerance on day coordinates.
    pub day_tolerance: f64,
    pub horizon_days: u32,
}

impl Rules {
    pub fn new(registration: &Registration) -> Self {
        Self {
            criteria: registration.criteria,
            lambda_per_hour: registration.lambda(),
            ratio_tolerance: registration.tolerance.admissibility,
            day_tolerance: registration.tolerance.hours.max(1e-9),
            horizon_days: registration.calendar.days,
        }
    }

    /// One past the last observed day: the `observed_until_day` of a complete record.
    pub fn horizon_end(&self) -> f64 {
        f64::from(self.horizon_days) + 1.0
    }

    /// Day coordinates agree within the registered tolerance.
    pub fn same_day(&self, left: f64, right: f64) -> bool {
        (left - right).abs() <= self.day_tolerance
    }
}

/// Which registered usefulness criteria a scenario carries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Scope {
    /// Listed benign: depth floor, settled-interval floor and the median settled day apply.
    pub benign: bool,
    /// Extra per-unit depth every unit must retain on top of the benign floor.
    pub unit_floor: Option<u8>,
    /// Retention against the depths of the joint finish applies.
    pub retention: bool,
}

impl Scope {
    pub fn of(scenario: &Scenario) -> Self {
        Self {
            benign: scenario.benign,
            unit_floor: scenario.unit_depth_floor,
            retention: scenario.retention,
        }
    }
}
