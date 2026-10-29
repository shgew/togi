//! Pure two-ends climbing rules (`docs/spec/tuner-ruleset14.md`, §§2–8).
//! Callers supply exact-profile hold/certificate evidence and commit only successful applications.
//! Campaign charges and permanent stops survive firmware and tuning-phase changes.

mod campaign;
mod ceiling;
mod decide;
mod finish;
mod history;
mod restore;
mod retreat;
mod step;

pub use campaign::{Campaign, Charge, EventPhase, LEARNING_ALLOWANCE};
pub use ceiling::{Ceiling, Ceilings, EdgeState, EvidenceId};
pub use decide::{
    ApplyPrerequisites, Blocker, ClimbAction, ClimbDecision, ClimbInputs, HoldInput, decide,
};
pub use finish::{FinishBook, FinishReason, FinishWitness, StopReason, UnitState};
pub use history::{
    AncestryLink, ApplicationId, ClimbError, ClimbHistory, Cursor, LearningStep, RetreatRecord,
    StepKind, TransitionId,
};
pub use restore::{RestorationKind, RestorationProposal, SavedProfile};
pub use retreat::Retreat;
pub use step::{Proposal, propose};
