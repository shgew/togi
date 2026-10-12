use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use togi_journal::{CorePhase, Seq};
use togi_machine::{Condition, CoreId, CoreSet, PerCore, Regime, TargetOffset, WorkloadId};

use crate::dashboard::{CyclePlan, HuntPlan, SearchTurn, TrialRequirement};
use crate::phase_plan::PhasePlan;
use crate::{
    BiosProfile, Combination, FailurePoint, Profile, R7CoreStatus, State, TrialHistory, Window,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoreView {
    pub core: CoreId,
    pub phase: Option<CorePhase>,
    pub offset: TargetOffset,
    pub pass: Option<TargetOffset>,
    pub failure_point: Option<TargetOffset>,
    pub solo_limit: Option<TargetOffset>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TemperaturePeak {
    pub celsius: i32,
    pub seq: Seq,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckingView {
    pub profile: Option<Profile>,
    pub profile_seq: Option<Seq>,
    pub cycle: Option<NonZeroU32>,
    pub peak: Option<TemperaturePeak>,
    pub exposure: Vec<(WorkloadId, u32)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConstraintView {
    pub failure_points: PerCore<FailurePoint>,
    pub combinations: Vec<Combination>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeepeningCheck {
    pub regime: Regime,
    pub workload: WorkloadId,
    pub cores: CoreSet,
    pub condition: Condition,
    pub profile: Profile,
    pub needed: u32,
    pub passed: u32,
    pub failed: u32,
    pub running: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeepeningView {
    pub round: NonZeroU32,
    pub seq: Seq,
    pub target: Profile,
    pub profile: Profile,
    pub cores: CoreSet,
    pub checks: Vec<DeepeningCheck>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    pub cores: Vec<CoreView>,
    pub constraints: Option<ConstraintView>,
    pub checking: CheckingView,
    pub deepening: Option<DeepeningView>,
    pub cycle_plan: CyclePlan,
    pub search_turns: Vec<SearchTurn>,
    pub hunt_plan: Option<HuntPlan>,
    pub phase_plan: Option<PhasePlan>,
    pub bios_profile: Option<BiosProfile>,
    pub r7_status: Vec<R7CoreStatus>,
    pub clean_cycles: u32,
    pub first_result: bool,
    pub requirement: Option<TrialRequirement>,
    pub trial_history: Option<TrialHistory>,
}

impl State {
    pub fn project(&self) -> Projection {
        let run = self.run().filter(|_| !self.poisoned());
        let cores = run.map_or_else(Vec::new, |run| {
            run.topo
                .all()
                .iter()
                .map(|core| {
                    let state = run.core(core);
                    CoreView {
                        core,
                        phase: state.phase,
                        offset: state.offset,
                        pass: state.pass,
                        failure_point: run.failure_point(core),
                        solo_limit: state.solo_limit,
                    }
                })
                .collect()
        });
        let checking = run.map_or_else(CheckingView::default, |run| CheckingView {
            profile: run.checking.profile(),
            profile_seq: run.checking.seq(),
            cycle: run.checking.last_cycle(),
            peak: run.checking.temperature_peak().map(|peak| TemperaturePeak {
                celsius: peak.celsius,
                seq: peak.seq,
            }),
            exposure: run.evidence.exposures().collect(),
        });
        let deepening = run.and_then(|run| {
            let round = run.rounds.active()?;
            let checks = round
                .checks()
                .iter()
                .map(|check| {
                    let count = run
                        .evidence
                        .count(
                            check.class,
                            &check.profile,
                            Window::LiveOrCarried {
                                since: Some(check.since),
                            },
                            round.trials(),
                        )
                        .unwrap_or_default();
                    let running = run.flight.open.as_ref().is_some_and(|flight| {
                        flight.intent.round == Some(round.id())
                            && flight.class == check.class
                            && flight.profile == check.profile
                    });
                    DeepeningCheck {
                        regime: check.class.regime(),
                        workload: check.class.workload(),
                        cores: check.class.loaded(),
                        condition: check.condition,
                        profile: check.profile,
                        needed: u32::from(round.trials().get()),
                        passed: count.valid,
                        failed: run
                            .evidence
                            .live_failures(check.class, &check.profile, Some(check.since))
                            .unwrap_or(0),
                        running,
                    }
                })
                .collect();
            Some(DeepeningView {
                round: round.id(),
                seq: round.start(),
                target: *round.target(),
                profile: *round.profile(),
                cores: round.cores(),
                checks,
            })
        });
        Projection {
            cores,
            constraints: self.constraints().map(|constraints| ConstraintView {
                failure_points: *constraints.failure_points(),
                combinations: constraints.combinations().to_vec(),
            }),
            checking,
            deepening,
            cycle_plan: self.cycle_plan(),
            search_turns: self.search_turns(),
            hunt_plan: self.hunt_plan(),
            phase_plan: self.phase_plan(),
            bios_profile: self.bios_profile(),
            r7_status: self.r7_status(),
            clean_cycles: self.clean_cycles(),
            first_result: self.first_result(),
            requirement: self.requirement(),
            trial_history: self.trial_history(),
        }
    }
}
