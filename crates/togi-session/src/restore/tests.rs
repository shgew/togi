use togi_journal::Payload;
use togi_machine::{PerCore, TargetOffset};

use super::*;
use crate::session::{Control, Fault};
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
fn startup_readback_resolution_survives_replaying_the_journal() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    session.begin().unwrap();
    let receipt = session
        .appender
        .smu_intent(
            togi_journal::SmuCommand::Set {
                core: crate::testing::core(1),
                offset: togi_machine::TargetOffset::ZERO,
            },
            &[],
        )
        .unwrap();
    let intent = receipt.intent();
    drop(session);

    let mut resumed = bench.open();
    assert_eq!(resumed.start().unwrap(), Control::Continue);
    assert_eq!(resumed.preflight().unwrap(), Control::Continue);
    assert_eq!(resumed.sweep().unwrap(), Control::Continue);
    assert_eq!(resumed.reconcile().unwrap(), Control::Continue);
    assert!(resumed.appender.folds.facts.open.is_empty());
    assert_eq!(
        resumed
            .appender
            .recovery()
            .latest_unmatched_intent(resumed.appender.journal().boot()),
        None
    );
    assert!(resumed.appender.journal().events().iter().any(|event| {
        matches!(event.payload, Payload::SmuReadback(_)) && event.cause == [intent]
    }));
    crate::testing::assert_fold_consistent(&resumed.appender);
    drop(resumed);

    let reopened = bench.open();
    assert!(reopened.appender.folds.facts.open.is_empty());
    crate::testing::assert_fold_consistent(&reopened.appender);
}

#[test_case::test_case(false; "before sweep")]
#[test_case::test_case(true; "after sweep")]
fn an_append_failure_uses_the_existing_emergency_gate(after_sweep: bool) {
    use crate::emergency::{Response, Withheld};
    use togi_journal::fs::{Fault, Op};

    let bench = Bench::new(2);
    let mut session = if after_sweep {
        let mut session = swept(&bench);
        session.validated = Some(session.host.validate_smu().unwrap());
        session
    } else {
        let mut session = bench.open();
        session.start().unwrap();
        session
    };
    bench
        .mem
        .inject(Fault::before(Op::Write, 1).named("events.jsonl"));
    let error = session
        .appender
        .append(
            Payload::SessionWarning(togi_journal::SessionWarning {
                operation: "test append".into(),
                trial: None,
                error: "test".into(),
            }),
            &[],
        )
        .unwrap_err();
    let ended = session.close(Err(error.into()));
    assert!(matches!(
        ended.errors.first(),
        Some(crate::SessionError::Journal(_))
    ));
    let emergency = ended.emergency.unwrap();
    if after_sweep {
        assert!(matches!(emergency.response, Response::Zeroed(_)));
    } else {
        assert!(matches!(
            emergency.response,
            Response::Withheld(Withheld::NoSweep)
        ));
    }
    assert!(
        ended
            .journal
            .events()
            .iter()
            .all(|event| !matches!(event.payload, Payload::Shutdown(_)))
    );
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

#[test_case::test_case(false; "already dead")]
#[test_case::test_case(true; "dies during close")]
fn an_smu_dead_close_reports_stock_not_verified_without_restoration(during_close: bool) {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-10, -10]);
    bench.sim.fail_write(1);
    let stop = if during_close {
        session.record_shutdown(&crate::Stop::Signal);
        crate::Stop::Signal
    } else {
        assert!(matches!(session.restore(), Err(Flow::DeadEnd(_))));
        session.check_dead_end().unwrap().unwrap()
    };
    let ended = session.close(Ok(stop));
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert!(ended.stock_unverified);
    assert!(matches!(
        ended.stop,
        Some(crate::Stop::DeadEnd { ref dead_end, .. })
            if dead_end.condition == togi_journal::DeadEndCondition::Smu
    ));
    let events = ended.journal.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload, Payload::ProfileRestored(_)))
            .count(),
        0
    );
    let warning = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload, Payload::SessionWarning(warning)
                    if warning.operation == "restore stock before stopping"
            )
        })
        .unwrap();
    assert!(warning.msg.contains("stock not verified"));
    assert!(warning.msg.contains("current offsets unknown"));
    assert!(warning.seq < events.last().unwrap().seq);
    assert!(matches!(
        events.last().unwrap().payload,
        Payload::Shutdown(_)
    ));
}

#[test]
fn an_smu_failure_restoring_after_another_dead_end_keeps_the_stop_and_reports_uncertainty() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-10, -10]);
    let stop = session
        .dead_end(
            togi_journal::DeadEnd {
                condition: togi_journal::DeadEndCondition::Containment,
                core: None,
                detail: "workload containment failed".into(),
                action: None,
            },
            &[],
        )
        .unwrap();
    bench.sim.fail_write(1);
    let ended = session.close(Ok(stop.clone()));
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(stop));
    assert!(ended.stock_unverified);
    let events = ended.journal.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.payload, Payload::ProfileRestored(_)))
            .count(),
        0
    );
    let warning = events
        .iter()
        .find(|event| {
            matches!(
                &event.payload, Payload::SessionWarning(warning)
                    if warning.operation == "restore stock before stopping"
            )
        })
        .unwrap();
    assert!(warning.msg.contains("stock not verified"));
    assert!(warning.msg.contains("current offsets unknown"));
    let shutdown = events.last().unwrap();
    assert!(matches!(shutdown.payload, Payload::Shutdown(_)));
    assert!(warning.seq < shutdown.seq);
}
