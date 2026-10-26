//! Adequate exposure for coverage (`docs/spec/tuner.md`, Adequate exposure).

use togi_machine::Regime;

use crate::Secs;

/// Seconds an R1–R7 launch warms up inside its intended duration before exposure counts.
pub const WARM_UP_S: u32 = 5;
/// Seconds an R8 or R9 launch warms up inside its intended duration before exposure counts.
pub const ROLE_WARM_UP_S: u32 = 30;

/// The warm-up boundary of `regime`: part of the intended duration, never added to it.
#[must_use]
pub const fn warm_up_secs(regime: Regime) -> u32 {
    match regime {
        Regime::R1
        | Regime::R2
        | Regime::R3
        | Regime::R4
        | Regime::R5
        | Regime::R6
        | Regime::R7 => WARM_UP_S,
        Regime::R8 | Regime::R9 => ROLE_WARM_UP_S,
    }
}

/// Whether a launch's outcome could be judged.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    Conclusive,
    Inconclusive,
}

/// Whether managed placement was valid for the whole launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementValidity {
    Valid,
    Withheld,
}

/// Whether the tested position earned passing credit (a host core never does).
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Credit {
    Granted,
    Withheld,
}

/// One launch of a class at an offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Launch {
    pub regime: Regime,
    pub intended: Secs,
    /// Seconds the launch ran from timing start, warm-up included.
    pub ran: u32,
    pub conclusion: Conclusion,
    pub placement: PlacementValidity,
    pub credit: Credit,
}

impl Launch {
    /// Whether the launch ran its whole intended duration and that duration outlasts the warm-up
    /// boundary, so it has eligible seconds.
    #[must_use]
    pub fn completed(&self) -> bool {
        self.ran >= self.intended.get() && self.intended.get() > warm_up_secs(self.regime)
    }

    /// Post-warm-up seconds this launch can add to exposure, ignoring placement and credit.
    #[must_use]
    pub fn eligible_seconds(&self) -> u32 {
        self.ran
            .min(self.intended.get())
            .saturating_sub(warm_up_secs(self.regime))
    }

    fn adequate(&self) -> bool {
        self.completed()
            && self.conclusion == Conclusion::Conclusive
            && self.placement == PlacementValidity::Valid
            && self.credit == Credit::Granted
    }
}

/// A class/offset is adequately exposed for coverage once at least one conclusive launch has
/// completed its intended duration, including its warm-up boundary, with valid placement and
/// credit. Truncated, inconclusive or uncredited launches, however many, never add up to it.
pub fn adequately_exposed<'a>(launches: impl IntoIterator<Item = &'a Launch>) -> bool {
    launches.into_iter().any(Launch::adequate)
}
