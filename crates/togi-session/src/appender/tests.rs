use jiff::tz::TimeZone;
use togi_journal::{Activity, Payload, TrialId, TrialIntent};
use togi_machine::{Condition, CoreSet, PerCore, Regime, pick_workload};
use togi_render::Renderer;

use super::Appender;
use crate::error::{AppendFailure, FoldError};
use crate::testing::{Rig, assert_fold_consistent, core, target};

fn unscheduled() -> Payload {
    Payload::TrialIntent(TrialIntent {
        trial: TrialId::from("0001".to_owned()),
        kernel_cursor: None,
        kernel_error: None,
        profile: PerCore::default(),
        core: Some(core(0)),
        cores: CoreSet::default(),
        offset: Some(target(-10)),
        regime: Regime::R1,
        workload: pick_workload(Regime::R1, 0).id(),
        duration_s: 60,
        condition: Condition::Alone,
        activity: Some(Activity::Search),
        retry: false,
        cycle: None,
        step: None,
        hunt: None,
        group: None,
        round: None,
        rerun: false,
    })
}

#[test]
fn every_append_folds_into_the_recovery_the_tuner_and_the_defect_state() {
    let mut rig = Rig::new(2);
    rig.start_at_zero();
    assert!(rig.appender.tuner().topology().is_some());
    assert_fold_consistent(&rig.appender);
}

#[test]
fn an_event_the_tuner_never_planned_is_a_corrupt_journal_now_and_on_resume() {
    let mut rig = Rig::new(2);
    let failure = rig.appender.append(unscheduled(), &[]).unwrap_err();
    let AppendFailure::Fold(FoldError::Tuner { seq, .. }) = failure else {
        panic!("{failure:?}");
    };
    assert_eq!(rig.events().last().map(|event| event.seq), Some(seq));
    let clock = rig.sim.seams().clock;
    let resumed = Appender::resume(
        rig.appender.into_journal(),
        clock,
        Renderer::default(),
        TimeZone::UTC,
    );
    assert!(matches!(
        resumed,
        Err(FoldError::Tuner { seq: at, .. }) if at == seq
    ));
}
