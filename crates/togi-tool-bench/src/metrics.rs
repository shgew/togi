use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
use std::time::Duration;

use togi_facts::Observation;
use togi_journal::{CycleEvent, Event, Payload, TrialId};
use togi_machine::{
    CoreId, CoreSet, ObservedOffset, PerCore, REGIMES, Regime, TargetOffset, workloads,
};
use togi_requests::TIE_V;
use togi_sim::replay::Replay;
use togi_sim::{Load, Model};

use crate::error::{MeasureError, RunError};
use crate::stats::share;

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
/// Time counts from the first `session.start`; trials, hunts, combinations and crashes count their events; real
/// answers count the decisive trials that journaled a `trial.start` and that `replay` holds an answer for; the
/// final profile is the last `profile.change`, the tuner's checking profile, or, before one, the session's
/// baseline; hazards are the model's steady-state failures per hour at the final profile.
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
    let final_profile = final_profile(events, cores, facts)?;
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
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
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
            let mut loaded: Vec<CoreId> = (0..CORES_PER_CCD)
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn cores(ids: &[i64]) -> Vec<CoreId> {
        ids.iter().map(|&id| CoreId::new(id).unwrap()).collect()
    }

    fn lanes(requests: &[(usize, f32)]) -> [f32; R7_CORES] {
        let mut lanes = [0.0; R7_CORES];
        for &(core, request) in requests {
            lanes[core] = request;
        }
        lanes
    }

    fn ids(cores: &[CoreId]) -> Vec<u8> {
        cores.iter().map(|core| core.get()).collect()
    }

    #[test]
    fn the_partial_chain_drops_one_tie_group_at_a_time_down_to_two_cores() {
        let requests = lanes(&[
            (0, 1.3),
            (1, 1.2995),
            (2, 1.2),
            (3, 1.14),
            (4, 1.13),
            (5, 1.12),
            (6, 1.11),
            (7, 1.1),
        ]);
        let mut loaded = cores(&[0, 1, 2, 3, 4, 5, 6, 7]);
        let mut chain = Vec::new();
        while loaded.len() >= 2 {
            chain.push(ids(&loaded));
            loaded = next_partial(&loaded, &requests);
        }
        assert_eq!(
            chain,
            [
                vec![0, 1, 2, 3, 4, 5, 6, 7],
                vec![2, 3, 4, 5, 6, 7],
                vec![3, 4, 5, 6, 7],
                vec![4, 5, 6, 7],
                vec![5, 6, 7],
                vec![6, 7],
            ]
        );
        assert_eq!(ids(&loaded), [7]);
    }

    #[test]
    fn a_tie_is_never_split_to_reach_a_two_core_load() {
        let requests = lanes(&[(0, 1.3), (1, 1.2995), (7, 1.1)]);
        assert_eq!(ids(&next_partial(&cores(&[0, 1, 7]), &requests)), [7]);
    }

    #[test]
    fn negative_requests_still_shed_their_top_group() {
        let requests = lanes(&[(0, -0.022), (1, -0.0225), (2, -0.05)]);
        assert_eq!(ids(&next_partial(&cores(&[0, 1, 2]), &requests)), [2]);
    }

    #[test]
    fn all_cores_within_the_tie_leave_nothing() {
        let requests = lanes(&[(0, 1.0), (1, 1.0005)]);
        assert!(next_partial(&cores(&[0, 1]), &requests).is_empty());
    }

    pub(crate) mod rail {
        use togi_sim::MachineDefinition;
        use togi_sim::definition::{CoreDef, SharedVoltageDef, VoltageCoreDef, VoltageWorkloadDef};

        use super::*;

        pub(crate) fn definition() -> MachineDefinition {
            let core = VoltageCoreDef {
                base_v: 1.072,
                threshold_v: 1.08,
                count_v: 0.0036,
                clock_v_per_100mhz: 0.0,
                threshold_clock_v_per_100mhz: 0.0,
                signals: None,
            };
            let workload = VoltageWorkloadDef {
                reference_mhz: 5000.0,
                full_mhz: [5000.0, 5000.0],
                idle_gain_mhz: 0.0,
                watts_per_core: 1.0,
                offset_watts_per_count: 0.0,
                package_mhz_per_w: 0.0,
                balance_mhz_per_w: 0.0,
                core: vec![core; R7_CORES],
            };
            MachineDefinition {
                core: (0..16)
                    .map(|id| CoreDef {
                        id,
                        alone: [0; 5],
                        together: [0; 7],
                        flat: 0.0,
                        idle: None,
                        workload: BTreeMap::new(),
                    })
                    .collect(),
                shared_voltage: Some(SharedVoltageDef {
                    idle_v: 0.8,
                    margin_v: 0.01,
                    rate: 1.0 / 3600.0,
                    background_rate: 0.0,
                    power_limit_w: 1000.0,
                    thermal_limit_w: 1000.0,
                    workload: workloads(Regime::R7)
                        .iter()
                        .map(|w| (w.id.to_owned(), workload.clone()))
                        .collect(),
                }),
                ..MachineDefinition::default()
            }
        }

        pub(crate) fn each_workload(
            definition: &mut MachineDefinition,
            mut change: impl FnMut(&str, &mut VoltageWorkloadDef),
        ) {
            for (id, workload) in &mut definition.shared_voltage.as_mut().unwrap().workload {
                change(id, workload);
            }
        }

        pub(crate) fn profile() -> Vec<TargetOffset> {
            vec![TargetOffset::clamped(-20); R7_CORES]
        }
    }

    fn hazard_per_h(model: &Model, profile: &[TargetOffset], workload: &str, ids: &[i64]) -> f64 {
        let load = Load {
            regime: Regime::R7,
            workload,
            cores: cores(ids).into_iter().collect(),
            duration: Duration::ZERO,
        };
        model.hazard(profile, &load).unwrap() * 3600.0
    }

    fn assert_close(got: f64, want: f64) {
        assert!(
            (got - want).abs() <= 1e-12 * want.abs().max(1.0),
            "{got} != {want}"
        );
    }

    #[test]
    fn a_partial_load_more_hazardous_than_its_ccd_is_the_worst_including_idle_hazard() {
        let mut definition = rail::definition();
        rail::each_workload(&mut definition, |_, w| {
            w.core[0].base_v = 1.272;
            w.core[1].base_v = 1.2715;
            for core in &mut w.core[8..] {
                core.base_v = 1.322;
            }
        });
        definition.core[15].idle = Some(-10);
        definition.model.past_limit_rate = 0.0002;
        definition.model.growth = 1.0;
        let model = Model::new(&definition, 1).unwrap();
        let profile = rail::profile();
        let workload = workloads(Regime::R7)[0].id;
        let partial = hazard_per_h(&model, &profile, workload, &[2, 3, 4, 5, 6, 7]);
        let full = hazard_per_h(&model, &profile, workload, &[0, 1, 2, 3, 4, 5, 6, 7]);
        assert!(
            partial > full,
            "the fixture must make the partial the worse"
        );
        let worst = worst_r7_hazard_per_h(&model, &profile).unwrap().unwrap();
        assert_close(worst, partial);
    }

    #[test]
    fn the_chain_asks_the_model_for_the_requests_of_every_shrunken_load() {
        let mut definition = rail::definition();
        rail::each_workload(&mut definition, |id, w| {
            for core in &mut w.core {
                core.base_v = 1.572;
            }
            if id == workloads(Regime::R7)[0].id {
                w.idle_gain_mhz = 100.0;
                let requests = [1.3, 1.2995, 1.1, 1.14, 1.13, 1.12, 1.11, 1.1];
                for (core, request) in requests.iter().enumerate() {
                    w.core[core].base_v = request + 0.072;
                }
                w.core[2].clock_v_per_100mhz = 0.03;
            }
        });
        let model = Model::new(&definition, 1).unwrap();
        let profile = rail::profile();
        let workload = workloads(Regime::R7)[0].id;
        let mut loaded = cores(&[0, 1, 2, 3, 4, 5, 6, 7]);
        let mut chain = Vec::new();
        while loaded.len() >= 2 {
            chain.push(ids(&loaded));
            let load = Load {
                regime: Regime::R7,
                workload,
                cores: loaded.iter().copied().collect(),
                duration: Duration::ZERO,
            };
            let lanes = model.r7_requests(&profile, &load).unwrap().unwrap();
            loaded = next_partial(&loaded, &lanes);
        }
        assert_eq!(
            chain,
            [
                vec![0, 1, 2, 3, 4, 5, 6, 7],
                vec![2, 3, 4, 5, 6, 7],
                vec![3, 4, 5, 6, 7],
                vec![4, 5, 6, 7],
                vec![5, 6, 7],
                vec![6, 7],
            ]
        );
        assert_eq!(ids(&loaded), [7]);
        let load = Load {
            regime: Regime::R7,
            workload,
            cores: cores(&[0, 1, 7]).into_iter().collect(),
            duration: Duration::ZERO,
        };
        let lanes = model.r7_requests(&profile, &load).unwrap().unwrap();
        assert_eq!(ids(&next_partial(&cores(&[0, 1, 7]), &lanes)), [7]);
        let worst = worst_r7_hazard_per_h(&model, &profile).unwrap().unwrap();
        assert_close(worst, hazard_per_h(&model, &profile, workload, &[6, 7]));
    }

    #[derive(Clone, Copy)]
    enum WorstLoad {
        SecondCcdPartial,
        AllCorePressure,
        LastWorkload,
    }

    fn worst_of(load: WorstLoad) -> (f64, f64) {
        let mut definition = rail::definition();
        let all = workloads(Regime::R7);
        let (selected, pressure) = match load {
            WorstLoad::SecondCcdPartial => (all[0].id, false),
            WorstLoad::AllCorePressure => (all[0].id, true),
            WorstLoad::LastWorkload => (all[all.len() - 1].id, false),
        };
        rail::each_workload(&mut definition, |id, w| {
            for core in &mut w.core {
                core.base_v = 1.472;
            }
            if pressure {
                w.full_mhz = [5000.0, 5200.0];
                w.package_mhz_per_w = 100.0;
                for core in &mut w.core {
                    core.base_v = 1.272;
                    core.clock_v_per_100mhz = 0.02;
                }
            } else if id == selected {
                w.core[8].base_v = 1.372;
                for core in &mut w.core[9..] {
                    core.base_v = 1.072;
                }
            }
        });
        if pressure {
            definition.shared_voltage.as_mut().unwrap().power_limit_w = 12.0;
        }
        let model = Model::new(&definition, 1).unwrap();
        let profile = rail::profile();
        let loaded: Vec<i64> = if pressure {
            (0..16).collect()
        } else {
            (9..16).collect()
        };
        let want = hazard_per_h(&model, &profile, selected, &loaded);
        let worst = worst_r7_hazard_per_h(&model, &profile).unwrap().unwrap();
        (worst, want)
    }

    #[test]
    fn the_worst_comes_from_a_partial_of_the_second_ccd() {
        let (worst, want) = worst_of(WorstLoad::SecondCcdPartial);
        assert_close(worst, want);
    }

    #[test]
    fn the_worst_comes_from_the_all_core_load_under_power_pressure() {
        let (worst, want) = worst_of(WorstLoad::AllCorePressure);
        assert_close(worst, want);
    }

    #[test]
    fn the_worst_comes_from_the_last_r7_workload_alone() {
        let (worst, want) = worst_of(WorstLoad::LastWorkload);
        assert_close(worst, want);
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod snapshot_tests;
