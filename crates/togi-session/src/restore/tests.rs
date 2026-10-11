use togi_journal::{CoreTransition, Payload};
use togi_machine::{PerCore, TargetOffset};
use togi_tuner::FailurePoint;

use super::*;
use crate::testing::{
    Bench, core, kinds_of, observed, offsets, put, recorded_baseline, swept, target,
};

#[test]
fn the_baseline_is_kept_no_deeper_than_the_tuner_holds_nor_than_a_failure_point() {
    let cores: CoreSet = [core(0), core(1), core(2)].into_iter().collect();
    let points: PerCore<FailurePoint> = [(core(2), FailurePoint::new(target(-7)))]
        .into_iter()
        .collect();
    let restored = restore_target(
        &observed(&[-20, -3, -30]),
        &offsets(&[-10, -8, -40]),
        Some(&points),
        cores,
    );
    assert_eq!(restored, offsets(&[-10, -3, -6]));
}

#[test]
fn a_baseline_no_write_could_have_set_is_clamped_and_a_missing_one_is_zero() {
    let cores: CoreSet = [core(0), core(1)].into_iter().collect();
    let restored = restore_target(&observed(&[7]), &PerCore::default(), None, cores);
    assert_eq!(restored, offsets(&[0, 0]));
}

fn restorations(session: &Session) -> Vec<PerCore<TargetOffset>> {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::ProfileRestored(restored) => Some(restored.offsets),
            _ => None,
        })
        .collect()
}

#[test]
fn the_machine_goes_back_to_the_baseline_and_the_restoration_cites_it() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-10, -12]);
    let baseline = session.appender().recovery().baseline().unwrap().0;
    assert_eq!(
        session.restore(&offsets(&[0, 0])).unwrap(),
        Restoration::Restored
    );
    assert_eq!(restorations(&session), [offsets(&[0, 0])]);
    let intents: Vec<_> = session
        .appender()
        .journal()
        .events()
        .iter()
        .skip_while(|event| !matches!(event.payload, Payload::ProfileApplied(_)))
        .filter(|event| matches!(event.payload, Payload::SmuIntent(_)))
        .collect();
    assert!(!intents.is_empty());
    assert!(intents.iter().all(|event| event.cause == [baseline]));
    assert_eq!(session.hardware.profile(), Some(offsets(&[0, 0])));
    crate::testing::assert_fold_consistent(session.appender());
}

#[test]
fn the_restoration_stops_at_what_the_tuner_still_holds() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[-20, -20]);
    put(&mut session, &[-30, -30]);
    session.restore(&offsets(&[-10, -25])).unwrap();
    assert_eq!(session.hardware.profile(), Some(offsets(&[-10, -20])));
}

#[test]
fn a_failure_point_the_journal_holds_bounds_the_restoration() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[-30, -30]);
    put(&mut session, &[-40, -40]);
    session
        .appender
        .append(
            Payload::CorePhase(CoreTransition {
                core: core(0),
                from: None,
                to: togi_journal::CorePhase::AtLimit,
                offset: target(-6),
                pass: Some(target(-6)),
                failure_point: Some(target(-7)),
                check_solo_limit: false,
                workloads: Vec::new(),
                cleared_combination: Vec::new(),
                reason: "test".to_owned(),
            }),
            &[],
        )
        .unwrap();
    session.restore(&offsets(&[-30, -30])).unwrap();
    assert_eq!(session.hardware.profile(), Some(offsets(&[-6, -30])));
}

#[test]
fn a_machine_the_run_never_touched_is_left_alone() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[-5, -5]);
    let before = kinds_of(session.appender());
    assert_eq!(
        session.restore(&offsets(&[0, 0])).unwrap(),
        Restoration::Unchanged
    );
    assert_eq!(kinds_of(session.appender()), before);
}

#[test]
fn a_profile_that_is_already_the_target_records_nothing() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[-10, -10]);
    put(&mut session, &[-10, -10]);
    let before = kinds_of(session.appender());
    assert_eq!(
        session.restore(&offsets(&[-10, -10])).unwrap(),
        Restoration::Unchanged
    );
    assert_eq!(kinds_of(session.appender()), before);
}

#[test]
fn without_the_proof_that_nothing_runs_nothing_is_written() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-10, -10]);
    session.stopped = None;
    let before = kinds_of(session.appender());
    assert_eq!(
        session.restore(&offsets(&[0, 0])).unwrap(),
        Restoration::Withheld
    );
    assert_eq!(kinds_of(session.appender()), before);
}

#[test]
fn a_crash_during_the_restoration_marks_the_machine_out_of_reach() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-10, -10]);
    bench.sim.crash_at_write(1);
    let flow = session.restore(&offsets(&[0, 0])).unwrap_err();
    assert!(matches!(flow, Flow::Fatal(ref error) if error.is_crash()));
    assert_eq!(session.fault, Some(Fault::Crashed));
    assert_eq!(session.hardware.profile(), None);
}
