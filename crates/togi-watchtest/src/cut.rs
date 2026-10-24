use std::collections::HashMap;

use togi_journal::{Activity, Event, EventKind, Outcome, Payload, TrialId, TrialIntent};
use togi_tuner::State;

use crate::Error;

/// The events up to and including the first one `accept` takes.
pub fn cut_at(events: &[Event], mut accept: impl FnMut(&Event) -> bool) -> Result<&[Event], Error> {
    events
        .iter()
        .position(&mut accept)
        .map(|at| &events[..=at])
        .ok_or(Error::NoCut("no event the cut accepts"))
}

/// Folds the events through a tuner state and cuts at the first event after which `accept` holds.
pub fn cut_when(
    events: &[Event],
    mut accept: impl FnMut(&State, &Event) -> bool,
) -> Result<&[Event], Error> {
    let mut state = State::new();
    for (at, event) in events.iter().enumerate() {
        state.fold(event)?;
        if accept(&state, event) {
            return Ok(&events[..=at]);
        }
    }
    Err(Error::NoCut("no event after which the tuner state accepts"))
}

/// Cuts at the `trial.start` of the first trial whose intent `accept` takes.
pub fn cut_trial(
    events: &[Event],
    mut accept: impl FnMut(&TrialIntent) -> bool,
) -> Result<&[Event], Error> {
    let mut chosen: Option<TrialId> = None;
    cut_at(events, |event| {
        match &event.payload {
            Payload::TrialIntent(intent) => {
                if chosen.is_none() && accept(intent) {
                    chosen = Some(intent.trial.clone());
                }
            }
            Payload::TrialStart(start) => return chosen.as_ref() == Some(&start.trial),
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
        false
    })
}

/// Cuts just after the first checking trial that passed: the point between that trial and the next, which the first
/// checking trial itself may not be, since it can end in a crash that a later boot records.
pub fn cut_passed_checking_trial(events: &[Event]) -> Result<&[Event], Error> {
    cut_passed_trial(events, |intent| intent.activity == Some(Activity::Checking))
}

/// Cuts just after the first trial whose intent `accept` takes and that passed.
pub fn cut_passed_trial(
    events: &[Event],
    mut accept: impl FnMut(&TrialIntent) -> bool,
) -> Result<&[Event], Error> {
    let mut accepted: HashMap<TrialId, bool> = HashMap::new();
    cut_at(events, |event| {
        match &event.payload {
            Payload::TrialIntent(intent) => {
                accepted.insert(intent.trial.clone(), accept(intent));
            }
            Payload::TrialEnd(end) => {
                return end.outcome == Outcome::Pass
                    && accepted.get(&end.trial).copied().unwrap_or(false);
            }
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
        false
    })
}

/// Cuts at the first trial start of a hunt run while the BIOS profile is confirmed and a core shows an unconfirmed
/// offset: the BIOS rows must stay beside the hunt panel.
pub fn hunt_with_unconfirmed_profile(events: &[Event]) -> Result<&[Event], Error> {
    let mut hunt: HashMap<TrialId, bool> = HashMap::new();
    cut_when(events, |state, event| {
        match &event.payload {
            Payload::TrialIntent(intent) => {
                hunt.insert(
                    intent.trial.clone(),
                    intent.activity == Some(Activity::Hunt),
                );
            }
            Payload::TrialStart(start) => {
                return hunt.get(&start.trial).copied().unwrap_or(false)
                    && state.bios_profile().is_some_and(|bios| {
                        bios.confirmed.is_some() && !bios.unconfirmed.is_empty()
                    });
            }
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
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
