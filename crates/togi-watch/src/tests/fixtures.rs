//! Concrete view fixtures shared by model, narration, panel and whole-frame tests.
//! Callers override the fields that define their scenario, leaving the common field inventory here.

use std::time::Duration;

use jiff::Timestamp;
use togi_journal::HuntStage;
use togi_machine::{CcdId, Condition, CoreSet, ObservedOffset, PerCore, Regime, workloads};
use togi_tuner::{GroupState, HuntGroup, HuntPlan, Lengths, Quota, RerunPlan};

use super::kit::{core, off, seq, set};
use crate::model::{CoreState, CoreView, Count, HuntCause, HuntFacts, TrialView};

pub(crate) fn core_view(id: u8, ccd: u8, state: CoreState) -> CoreView {
    CoreView {
        id: core(id),
        ccd: CcdId::try_from(ccd).unwrap(),
        state,
        queued: None,
        applied: Some(ObservedOffset(0)),
        profile: off(0),
        solo: None,
        pass: None,
        next: None,
        fail: None,
        holder: None,
        gave_back: None,
        group_fails: Vec::new(),
        returns_to: None,
        confirm: None,
        loaded: false,
        judged: false,
    }
}

pub(crate) fn trial_view(regime: Regime) -> TrialView {
    TrialView {
        id: "t1".to_owned().into(),
        regime,
        workload: workloads(regime)[0].id(),
        cores: CoreSet::default(),
        parked: CoreSet::default(),
        idle: CoreSet::default(),
        condition: Condition::Together,
        activity: None,
        profile: PerCore::default(),
        started: None,
        crashed: false,
        duration: Duration::from_secs(120),
        partial: false,
        rerun: false,
        retry: false,
        cycle: None,
        hunt: None,
        core: None,
        offset: None,
        round: None,
        count: Count::default(),
    }
}

pub(crate) fn hunt_group(number: u32, cores: &[u8], outcome: GroupState) -> HuntGroup {
    HuntGroup {
        number,
        cores: set(cores),
        profile: PerCore::default(),
        stage: HuntStage::Full,
        probe: None,
        held: Vec::new(),
        outcome,
        passed: 0,
        needed: 5,
        carried: false,
        running: outcome == GroupState::Running,
    }
}

pub(crate) fn hunt_plan(candidates: &[u8]) -> HuntPlan {
    HuntPlan {
        number: 1,
        regime: Regime::R7,
        failure: seq(1),
        trial: None,
        candidates: set(candidates),
        cut: 0,
        parts: Vec::new(),
        groups: Vec::new(),
        probes: Vec::new(),
        rerun: RerunPlan {
            regime: Regime::R7,
            cores: CoreSet::default(),
            lengths: Lengths {
                short: Quota {
                    count: 1,
                    secs: 600,
                },
                long: None,
            },
        },
    }
}

pub(crate) fn hunt_cause(trial_number: u32) -> HuntCause {
    HuntCause {
        at: None,
        trial: None,
        end: None,
        signal: None,
        core: None,
        known: false,
        carried: false,
        regime: None,
        rerun_of: false,
        trial_number,
    }
}

pub(crate) fn hunt_facts(started: Option<Timestamp>) -> HuntFacts {
    HuntFacts {
        started,
        cause: hunt_cause(0),
        parking: None,
        group_signals: Vec::new(),
    }
}
