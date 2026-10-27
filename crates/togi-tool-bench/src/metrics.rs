use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
use std::time::Duration;

use togi_facts::Observation;
use togi_journal::{Event, Payload, TrialId};
use togi_machine::{
    CoreId, CoreSet, LEGACY_REGIMES, ObservedOffset, PerCore, Regime, TargetOffset,
};
use togi_sim::replay::Replay;
use togi_sim::{Load, Model};

use crate::error::{MeasureError, RunError};
use crate::stats::share;

/// What one session's journal says, in the units a record keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    pub sim_hours: f64,
    pub crashes: u32,
    pub trials: u32,
    pub real_answers: u32,
    pub real_answer_share: f64,
    pub trial_hours: f64,
    /// All-core steady-state failures/hour over R1 to R7 only; R8 and R9 are not yet scored.
    pub hazard_per_h: BTreeMap<Regime, f64>,
    pub hazard_max_per_h: f64,
}

/// Measures a session's `events` on `model`, the machine it ran on, with `replay`, the oracle that answered its
/// trials from real facts when it had one.
///
/// Time counts from the first `session.start`; trials and crashes count their events; real answers count the
/// decisive trials that journaled a `trial.start` and that `replay` holds an answer for; hazards are the model's
/// steady-state failures per hour at the profile the session ended on: the last `profile.change`, or, before
/// one, the session's baseline.
pub fn measure(
    events: &[Event],
    model: &Model,
    replay: Option<&Replay>,
) -> Result<Measured, RunError> {
    measure_with(events, model, replay, None)
}

pub(crate) fn measure_with(
    events: &[Event],
    model: &Model,
    replay: Option<&Replay>,
    facts: Option<&togi_journal::State>,
) -> Result<Measured, RunError> {
    let cores = model.cores();
    let profile = targets(&final_profile(events, cores, facts)?, cores);
    let mut measured = Measured {
        sim_hours: 0.0,
        crashes: 0,
        trials: 0,
        real_answers: 0,
        real_answer_share: 0.0,
        trial_hours: 0.0,
        hazard_per_h: BTreeMap::new(),
        hazard_max_per_h: 0.0,
    };
    count_events(events, &mut measured);
    measured.real_answers = real_answers(events, model, replay)?;
    measured.real_answer_share = share(measured.real_answers, measured.trials);
    let loaded: CoreSet = (0..cores).filter_map(core).collect();
    // Hazard scoring covers R1 to R7 only; R8 and R9 are not yet scored.
    for regime in LEGACY_REGIMES {
        let load = Load {
            regime,
            workload: "",
            cores: loaded,
            duration: Duration::ZERO,
        };
        let rate = model.hazard(&profile, &load).map_err(shape)? * 3600.0;
        measured.hazard_per_h.insert(regime, rate);
        measured.hazard_max_per_h = measured.hazard_max_per_h.max(rate);
    }
    Ok(measured)
}

fn shape(error: togi_sim::ShapeError) -> RunError {
    RunError::Measure(MeasureError::Shape(error))
}

fn core(index: usize) -> Option<CoreId> {
    CoreId::new(i64::try_from(index).ok()?).ok()
}

fn count_events(events: &[Event], measured: &mut Measured) {
    let mut start = None;
    for event in events {
        if start.is_none() && matches!(event.payload, Payload::SessionStart(_)) {
            start = Some(event.time);
        }
        let Some(start) = start else {
            continue;
        };
        let hours = event.time.duration_since(start).as_secs_f64() / 3600.0;
        measured.sim_hours = hours;
        match &event.payload {
            Payload::CrashDetected(_) => measured.crashes += 1,
            Payload::TrialEnd(end) => {
                measured.trials += 1;
                measured.trial_hours += f64::from(end.duration_s) / 3600.0;
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
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::HuntStart(_)
            | Payload::Combination(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::TaskStart(_)
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
            | Payload::DayFault(_)
            | Payload::SessionImported(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayApplied(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayImported(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
    }
}

fn final_profile(
    events: &[Event],
    cores: usize,
    facts: Option<&togi_journal::State>,
) -> Result<PerCore<ObservedOffset>, RunError> {
    let checking = events.iter().rev().find_map(|event| {
        if let Payload::ProfileChange(change) = &event.payload {
            Some(change.to)
        } else {
            None
        }
    });
    let baseline = match checking {
        Some(_) => None,
        None if facts.is_some() => {
            facts.and_then(|facts| facts.session.as_ref().map(|session| session.baseline))
        }
        None => togi_journal::State::replay(events)
            .map_err(|error| RunError::Measure(MeasureError::Replay(error)))?
            .session
            .map(|session| session.baseline),
    };
    Ok((0..cores)
        .filter_map(core)
        .map(|core| {
            let offset = match checking {
                Some(profile) => profile.get(core).map(TargetOffset::observed),
                None => baseline.and_then(|baseline| baseline.get(core)),
            };
            (core, offset.unwrap_or_default())
        })
        .collect())
}

fn targets(profile: &PerCore<ObservedOffset>, cores: usize) -> Vec<TargetOffset> {
    (0..cores)
        .map(|index| {
            core(index)
                .and_then(|core| profile.get(core))
                .map_or(TargetOffset::ZERO, |offset| TargetOffset::clamped(offset.0))
        })
        .collect()
}

fn real_answers(events: &[Event], model: &Model, replay: Option<&Replay>) -> Result<u32, RunError> {
    let Some(replay) = replay else {
        return Ok(0);
    };
    let Some(session) = events.iter().find_map(|event| {
        if let Payload::SessionStart(start) = &event.payload {
            Some(start.session)
        } else {
            None
        }
    }) else {
        return Ok(0);
    };
    let facts = togi_facts::extract(session, events.iter().cloned().map(Ok::<_, Infallible>))
        .map_err(|error| RunError::Measure(MeasureError::Facts(error)))?;
    let started: HashSet<&TrialId> = events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialStart(start) = &event.payload {
                Some(&start.trial)
            } else {
                None
            }
        })
        .collect();
    let mut answered = 0;
    for fact in &facts.facts {
        let Observation::Trial { class, profile, .. } = &fact.observation else {
            continue;
        };
        if !fact
            .source
            .trial
            .as_ref()
            .is_some_and(|trial| started.contains(trial))
        {
            continue;
        }
        let load = Load {
            regime: class.workload.regime(),
            workload: class.workload.as_str(),
            cores: class.loaded,
            duration: Duration::from_secs(u64::from(class.duration_s)),
        };
        let applied: Vec<TargetOffset> = (0..model.cores())
            .map(|index| {
                core(index)
                    .and_then(|core| profile.get(core))
                    .unwrap_or(TargetOffset::ZERO)
            })
            .collect();
        if replay.has_answer(model.bios_context(), &load, &applied) {
            answered += 1;
        }
    }
    Ok(answered)
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod snapshot_tests;
