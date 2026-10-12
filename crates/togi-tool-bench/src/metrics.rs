use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::Duration;

use togi_facts::Observation;
use togi_journal::{CycleEvent, Event, Payload};
use togi_machine::{
    CoreId, CoreSet, ObservedOffset, PerCore, REGIMES, Regime, TargetOffset, workloads,
};
use togi_requests::TIE_V;
use togi_sim::replay::Replay;
use togi_sim::{Load, Model};

use crate::error::RunError;
use crate::stats::share;

const HALF_CCD: usize = 8;
const CORES_PER_CCD: usize = 8;
const R7_CORES: usize = 16;

/// What one session's journal says, in the units a record keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    pub sim_hours: f64,
    pub first_passed_cycle_h: Option<f64>,
    pub passed_cycles: u32,
    pub crashes: u32,
    pub trials: u32,
    pub real_answers: u32,
    pub real_answer_share: f64,
    pub trial_hours: f64,
    pub hunts: u32,
    pub combinations: u32,
    pub final_profile: PerCore<ObservedOffset>,
    pub depth: i64,
    pub hazard_per_h: BTreeMap<Regime, f64>,
    pub hazard_max_per_h: f64,
    pub worst_r7_hazard_per_h: Option<f64>,
}

/// Measures a session's `events` on `model`, the machine it ran on, with `replay`, the oracle that answered its
/// trials from real facts when it had one.
///
/// Time counts from the first `session.start`; trials, hunts, combinations and crashes count their events; the
/// final profile is the tuner's checking profile or, before it has one, the session's baseline; hazards are the
/// model's steady-state failures per hour at the final profile.
pub fn measure(
    events: &[Event],
    model: &Model,
    replay: Option<&Replay>,
) -> Result<Measured, RunError> {
    let cores = model.cores();
    let final_profile = final_profile(events, cores)?;
    let depth = final_profile.iter().map(|(_, offset)| offset.0).sum();
    let profile = targets(&final_profile, cores);
    let mut measured = Measured {
        sim_hours: 0.0,
        first_passed_cycle_h: None,
        passed_cycles: 0,
        crashes: 0,
        trials: 0,
        real_answers: 0,
        real_answer_share: 0.0,
        trial_hours: 0.0,
        hunts: 0,
        combinations: 0,
        final_profile,
        depth,
        hazard_per_h: BTreeMap::new(),
        hazard_max_per_h: 0.0,
        worst_r7_hazard_per_h: None,
    };
    count_events(events, &mut measured);
    measured.real_answers = real_answers(events, model, replay)?;
    measured.real_answer_share = share(measured.real_answers, measured.trials);
    let loaded: CoreSet = (0..cores).filter_map(core).collect();
    for regime in REGIMES {
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
    measured.worst_r7_hazard_per_h = worst_r7_hazard_per_h(model, &profile)?;
    Ok(measured)
}

fn shape(error: togi_sim::ShapeError) -> RunError {
    RunError::Measure(error.to_string())
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
            Payload::CheckingCycle(cycle) if cycle.event == CycleEvent::End && cycle.passed => {
                measured.passed_cycles += 1;
                measured.first_passed_cycle_h.get_or_insert(hours);
            }
            Payload::CrashDetected(_) => measured.crashes += 1,
            Payload::TrialEnd(end) => {
                measured.trials += 1;
                measured.trial_hours += f64::from(end.duration_s) / 3600.0;
            }
            Payload::HuntStart(_) => measured.hunts += 1,
            Payload::Combination(_) => measured.combinations += 1,
            _ => {}
        }
    }
}

fn final_profile(events: &[Event], cores: usize) -> Result<PerCore<ObservedOffset>, RunError> {
    let tuner = togi_tuner::replay(events)
        .map_err(|error| RunError::Measure(format!("replay the tuner: {error}")))?;
    if let Some(profile) = tuner.profile() {
        return Ok(profile
            .iter()
            .map(|(core, offset)| (core, offset.observed()))
            .collect());
    }
    let state = togi_journal::State::replay(events)
        .map_err(|error| RunError::Measure(format!("replay the journal: {error}")))?;
    let baseline = state.session.map(|session| session.baseline);
    Ok((0..cores)
        .filter_map(core)
        .map(|core| {
            let offset = baseline
                .and_then(|baseline| baseline.get(core))
                .unwrap_or_default();
            (core, offset)
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

fn real_answers(
    events: &[Event],
    model: &Model,
    replay: Option<&Replay>,
) -> Result<u32, RunError> {
    let Some(replay) = replay else {
        return Ok(0);
    };
    let Some(session) = events.iter().find_map(|event| match &event.payload {
        Payload::SessionStart(start) => Some(start.session),
        _ => None,
    }) else {
        return Ok(0);
    };
    let facts = togi_facts::extract(session, events.iter().cloned().map(Ok::<_, Infallible>))
        .map_err(|error| RunError::Measure(format!("extract the trial facts: {error}")))?;
    let mut answered = 0;
    for fact in &facts.facts {
        let Observation::Trial { class, profile, .. } = &fact.observation else {
            continue;
        };
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

/// The largest hazard per hour of the final profile over every R7 workload under the loads the tuner can
/// produce on a shared rail: each CCD's full load and its request-ordered partials of two or more cores, then
/// all cores. `None` for a machine without a shared rail.
fn worst_r7_hazard_per_h(model: &Model, profile: &[TargetOffset]) -> Result<Option<f64>, RunError> {
    if model.cores() != R7_CORES {
        return Ok(None);
    }
    let all: CoreSet = (0..R7_CORES).filter_map(core).collect();
    let mut worst = 0.0_f64;
    for workload in workloads(Regime::R7) {
        for ccd in 0..R7_CORES / CORES_PER_CCD {
            let mut loaded: Vec<CoreId> = (0..HALF_CCD)
                .filter_map(|index| core(ccd * CORES_PER_CCD + index))
                .collect();
            while loaded.len() >= 2 {
                let load = Load {
                    regime: Regime::R7,
                    workload: workload.id,
                    cores: loaded.iter().copied().collect(),
                    duration: Duration::ZERO,
                };
                let Some(lanes) = model.r7_requests(profile, &load).map_err(shape)? else {
                    return Ok(None);
                };
                worst = worst.max(model.hazard(profile, &load).map_err(shape)? * 3600.0);
                loaded = next_partial(&loaded, &lanes);
            }
        }
        let load = Load {
            regime: Regime::R7,
            workload: workload.id,
            cores: all,
            duration: Duration::ZERO,
        };
        worst = worst.max(model.hazard(profile, &load).map_err(shape)? * 3600.0);
    }
    Ok(Some(worst))
}

/// `cores` without the top tie group: the cores whose request is within [`TIE_V`] of the highest.
pub(crate) fn next_partial(cores: &[CoreId], lanes: &[f32; R7_CORES]) -> Vec<CoreId> {
    let top = cores
        .iter()
        .map(|core| f64::from(lanes[core.index()]))
        .fold(f64::NEG_INFINITY, f64::max);
    cores
        .iter()
        .copied()
        .filter(|core| f64::from(lanes[core.index()]) < top - TIE_V)
        .collect()
}
