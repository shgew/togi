use togi_journal::Payload;
use togi_machine::{PerCore, TargetOffset};

use super::*;
use crate::session::Fault;
use crate::testing::{Bench, core, kinds_of, offsets, put, recorded_baseline, swept};

#[test]
fn every_core_is_restored_to_zero_whatever_the_night_held() {
    let cores: CoreSet = [core(0), core(1), core(2)].into_iter().collect();
    assert_eq!(restore_target(cores), offsets(&[0, 0, 0]));
}

fn restorations(session: &Session) -> Vec<PerCore<TargetOffset>> {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::ProfileRestored(restored) = &event.payload {
                Some(restored.offsets)
            } else {
                None
            }
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
    assert_eq!(session.restore().unwrap(), Restoration::Restored);
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
fn a_machine_the_run_never_touched_is_left_alone() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    let before = kinds_of(session.appender());
    assert_eq!(session.restore().unwrap(), Restoration::Unchanged);
    assert_eq!(kinds_of(session.appender()), before);
}

#[test]
fn a_profile_that_is_already_the_target_records_nothing() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[0, 0]);
    let before = kinds_of(session.appender());
    assert_eq!(session.restore().unwrap(), Restoration::Unchanged);
    assert_eq!(kinds_of(session.appender()), before);
}

#[test]
fn without_the_proof_that_nothing_runs_nothing_is_written() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-10, -10]);
    session.lifecycle.lose_stopped();
    let before = kinds_of(session.appender());
    assert_eq!(session.restore().unwrap(), Restoration::Withheld);
    assert_eq!(kinds_of(session.appender()), before);
}

#[test]
fn a_crash_during_the_restoration_marks_the_machine_out_of_reach() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-10, -10]);
    bench.sim.crash_at_write(1);
    let flow = session.restore().unwrap_err();
    assert!(matches!(flow, Flow::Fatal(ref error) if error.is_crash()));
    assert_eq!(session.lifecycle.fault(), Some(Fault::Crashed));
    assert_eq!(session.hardware.profile(), None);
}
