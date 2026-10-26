use std::collections::HashMap;

use togi_journal::{Event, EventKind, Outcome, Payload, TrialId, TrialIntent};
use togi_tuner::policy::{Params, State};

use crate::Error;

/// The events up to and including the first one `accept` takes.
pub fn cut_at(events: &[Event], mut accept: impl FnMut(&Event) -> bool) -> Result<&[Event], Error> {
    events
        .iter()
        .position(&mut accept)
        .map(|at| &events[..=at])
        .ok_or(Error::NoCut("no event the cut accepts"))
}

/// Folds the events through the Ruleset 13 policy state at the specification's parameters and cuts at the first
/// event after which `accept` holds.
pub fn cut_when(
    events: &[Event],
    mut accept: impl FnMut(&State, &Event) -> bool,
) -> Result<&[Event], Error> {
    let mut state = State::new(Params::SPEC);
    for (at, event) in events.iter().enumerate() {
        state.fold(event)?;
        if accept(&state, event) {
            return Ok(&events[..=at]);
        }
    }
    Err(Error::NoCut(
        "no event after which the policy state accepts",
    ))
}

/// Cuts at the `trial.start` of the first trial whose intent `accept` takes.
pub fn cut_trial(
    events: &[Event],
    mut accept: impl FnMut(&TrialIntent) -> bool,
) -> Result<&[Event], Error> {
    let mut chosen: Option<TrialId> = None;
    cut_at(events, |event| {
        if let Payload::TrialIntent(intent) = &event.payload {
            if chosen.is_none() && accept(intent) {
                chosen = Some(intent.trial.clone());
            }
        } else if let Payload::TrialStart(start) = &event.payload {
            return chosen.as_ref() == Some(&start.trial);
        }
        false
    })
}

/// Cuts just after the first trial whose intent `accept` takes and that passed: the point between that trial and the
/// next, which the first such trial itself may not be, since it can end in a crash that a later boot records.
pub fn cut_passed_trial(
    events: &[Event],
    mut accept: impl FnMut(&TrialIntent) -> bool,
) -> Result<&[Event], Error> {
    let mut accepted: HashMap<TrialId, bool> = HashMap::new();
    cut_at(events, |event| {
        if let Payload::TrialIntent(intent) = &event.payload {
            accepted.insert(intent.trial.clone(), accept(intent));
        } else if let Payload::TrialEnd(end) = &event.payload {
            return end.outcome == Outcome::Pass
                && accepted.get(&end.trial).copied().unwrap_or(false);
        }
        false
    })
}

/// The journal a crashed session shows after the machine restarted and before `togi run` records the crash: it ends
/// with the events a later boot wrote first.
pub fn before_crash_detected(events: &[Event]) -> Result<&[Event], Error> {
    match events
        .iter()
        .position(|event| event.kind() == EventKind::CrashDetected)
    {
        Some(at) if at > 0 => Ok(&events[..at]),
        _ => Err(Error::NoCut("no crash in the simulated journal")),
    }
}
