use togi_journal::Event;
use togi_machine::CoreId;

use crate::state::Run;
use crate::{Action, DecisionAction, FoldError, State};

/// Why the tuner has no next action: the journal has not given it everything it needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NotReady {
    #[error("session.start has not been folded")]
    NotStarted,
    #[error("an earlier event could not be folded")]
    Poisoned,
    #[error("core {0} has no phase yet")]
    NoPhase(CoreId),
    #[error("no rule calls for an action")]
    NoAction,
}

/// Folds every event into a fresh state, stopping at the first the tuner cannot follow.
pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Result<State, FoldError> {
    let mut state = State::new();
    for event in events {
        state.fold(event)?;
    }
    Ok(state)
}

impl State {
    fn ready(&self) -> Result<&Run, NotReady> {
        if self.poisoned() {
            return Err(NotReady::Poisoned);
        }
        self.run().ok_or(NotReady::NotStarted)
    }

    /// The next decision, trial or ranking read the folded state calls for.
    ///
    /// It only reads: it returns the same answer until the caller records that action's event and
    /// folds it, so a caller loops by folding each answer before it asks again.
    ///
    /// ```compile_fail
    /// use togi_journal::Event;
    /// use togi_tuner::State;
    ///
    /// fn signatures_take_no_event_list(state: &State, events: &[Event]) {
    ///     let _ = state.next(events);
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use togi_journal::Event;
    /// use togi_tuner::State;
    ///
    /// fn project_takes_no_event_list(state: &State, events: &[Event]) {
    ///     let _ = state.project(events);
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use togi_journal::Event;
    /// use togi_tuner::State;
    ///
    /// fn forecast_takes_no_event_list(state: &State, events: &[Event]) {
    ///     let _ = state.forecast(events);
    /// }
    /// ```
    pub fn next(&self) -> Result<Action, NotReady> {
        self.ready()?.next()
    }

    /// The next decision that must be recorded before the tuner may stop: never a trial or a
    /// ranking read, and `None` once nothing is owed.
    ///
    /// It only reads: it returns the same decision until the caller records it and folds the event,
    /// so a `drain` loop folds each decision in its body; one that does not fold never ends.
    pub fn drain(&self) -> Option<DecisionAction> {
        self.ready().ok()?.drain()
    }
}
