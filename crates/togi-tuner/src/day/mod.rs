//! Ruleset 13's day-side policy (`docs/spec/tuner.md`, Day steps and Daily events): daily-event
//! classification, whole-profile vetoes, escalation, the applied-hour hold and the day step itself.
//!
//! Everything here is pure: callers pass the raw daily observations, answers, successful applications
//! and typed intervals together with the caps, qualifications and firmware validity the night side and
//! the fold provide, and receive a [`DayDecision`] with its witnesses. Nothing here writes hardware or
//! the journal, and every threshold is a [`DayParams`] field defaulting to the specification's value.

mod error;
mod events;
mod history;
mod hold;
mod profile;
mod retreat;
mod step;

pub use error::DayStepError;
pub use events::{
    Answer, Certainty, Classification, CpuCause, DailyEvent, EventId, MceKind, Observation,
    ProvisionalCause, QuestionState, Witness, classify, daily_events,
};
pub use history::{ApplicationKind, DayApplication, DayHistory, HistoryError, Promotion, Reverted};
pub use hold::{DayInterval, IntervalKind, Readback, credited_secs};
pub use profile::{CoreProfile, CoreProfileError};
pub use retreat::{
    EventStatus, ReproductionOutcome, RetreatCause, RetreatEvent, RetreatState, Veto,
    event_statuses, vetoes,
};
pub(crate) use retreat::{awaiting, shallower};
pub use step::{
    Blocker, DayDecision, DayInputs, Firmware, Freeze, HoldCredit, Invocation, Pause, Proposal,
    Restoration, day_step, next_proposal, propose,
};

/// Applied daily seconds at the exact current D a promotion needs: 50 hours (`docs/spec/tuner.md`,
/// Hold and next proposal).
pub const HOLD_SECS: u64 = 50 * 3600;

/// The day policy's thresholds, explicit so a simulator can vary them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DayParams {
    /// Applied seconds at the exact current D a promotion needs.
    pub hold_secs: u64,
}

impl DayParams {
    /// The specification's values.
    pub const SPEC: Self = Self {
        hold_secs: HOLD_SECS,
    };
}
