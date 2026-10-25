//! Concrete view fixtures shared by model, narration, panel and whole-frame tests.
//! Callers override the fields that define their scenario, leaving the common field inventory here.

use std::time::Duration;

use togi_machine::{CcdId, Condition, CoreSet, ObservedOffset, PerCore, Regime, workloads};

use super::kit::{core, off};
use crate::model::{CoreView, TrialView};

pub(crate) fn core_view(id: u8, ccd: u8) -> CoreView {
    CoreView {
        id: core(id),
        ccd: CcdId::try_from(ccd).unwrap(),
        applied: Some(ObservedOffset(0)),
        profile: off(0),
        loaded: false,
    }
}

pub(crate) fn trial_view(regime: Regime) -> TrialView {
    TrialView {
        id: "t1".to_owned().into(),
        regime,
        workload: workloads(regime)[0].id(),
        cores: CoreSet::default(),
        idle: CoreSet::default(),
        condition: Condition::Together,
        activity: None,
        task: None,
        tested: None,
        profile: PerCore::default(),
        started: None,
        crashed: false,
        duration: Duration::from_secs(120),
        retry: false,
    }
}
