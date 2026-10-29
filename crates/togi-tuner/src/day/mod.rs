//! Daily-event classification, exact-profile hold accounting and successful application facts.
//!
//! The unit-generic [`crate::climb`] engine owns decisions. Released CCD event/history adapters
//! remain available for decoding their original journal meaning, not as a selectable old policy.

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
pub use hold::{DayInterval, IntervalKind, Readback, credited_secs, hold_accounting};
pub use profile::{CoreProfile, CoreProfileError};
pub use retreat::{
    EventStatus, ReproductionOutcome, RetreatState, Veto, event_statuses, vetoes,
};
pub use step::{Firmware, Freeze, Invocation, Pause};

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
