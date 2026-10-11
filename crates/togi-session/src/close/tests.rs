use std::time::Duration;

use jiff::Timestamp;
use togi_journal::fs::{Fault, Op};
use togi_journal::{DeadEnd, DeadEndCondition, Event, Payload, SessionWarning, ShutdownReason};
use togi_machine::{
    BankType, BootId, CpuId, Error, Kernel, KernelCursor, KernelRead, Partial, PerCore,
    PstoreRecord, ResetReason, TargetOffset,
};
use togi_tuner::DecisionAction;

use crate::emergency::{Response, Withheld};
use crate::session::{Fault as Broken, Visit};
use crate::testing::{Bench, core, offsets, put, recorded_baseline, swept, target};
use crate::{Ended, Session, SessionError, Stop};

fn tuned(bench: &Bench) -> Session {
    let mut session = swept(bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-5, -5]);
    session.validated = Some(session.host.validate_smu().unwrap());
    session
}

fn signalled(session: &mut Session) -> Stop {
    session.record_shutdown(&Stop::Signal);
    Stop::Signal
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(|event| event.kind().as_str()).collect()
}

fn smu_offsets(bench: &Bench) -> Vec<i64> {
    let mut smu = bench.sim.seams().smu;
    (0..2).map(|n| smu.offset(core(n)).unwrap().0).collect()
}

fn held() -> PerCore<TargetOffset> {
    offsets(&[0, 0])
}

fn close(session: Session, run: Result<Stop, SessionError>) -> Ended {
    session.close(run, &held(), Vec::new())
}

fn dead_end(condition: DeadEndCondition) -> DeadEnd {
    DeadEnd {
        condition,
        core: None,
        detail: "test".into(),
        action: None,
    }
}

fn shutdown_reason(events: &[Event]) -> Option<ShutdownReason> {
    match &events.last()?.payload {
        Payload::Shutdown(shutdown) => Some(shutdown.reason),
        _ => None,
    }
}

#[test]
fn a_stop_restores_the_baseline_and_ends_with_the_shutdown() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    let ended = close(session, run);
    assert!(ended.errors.is_empty());
    assert_eq!(ended.stop, Some(Stop::Signal));
    assert!(ended.emergency.is_none());
    let events = ended.journal.events();
    let kinds = kinds(events);
    assert_eq!(kinds[kinds.len() - 2..], ["profile.restored", "shutdown"]);
    assert_eq!(shutdown_reason(events), Some(ShutdownReason::Signal));
    assert_eq!(smu_offsets(&bench), [0, 0]);
}

#[test]
fn an_error_still_restores_and_is_kept_first() {
    let bench = Bench::new(2);
    let session = tuned(&bench);
    let ended = close(session, Err(SessionError::NoSuchCore { core: core(9) }));
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::NoSuchCore { .. }]
    ));
    assert_eq!(ended.stop, None);
    let kinds = kinds(ended.journal.events());
    assert!(kinds.contains(&"profile.restored"));
    assert!(!kinds.contains(&"shutdown"));
    assert_eq!(smu_offsets(&bench), [0, 0]);
}

#[test]
fn a_cleanup_error_follows_the_error_that_ended_the_run() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    session.record_shutdown(&Stop::Signal);
    session.kernel = Box::new(Unreadable);
    let ended = close(session, Err(SessionError::NoSuchCore { core: core(9) }));
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::NoSuchCore { .. }, error] if error.is_crash()
    ));
    assert!(!kinds(ended.journal.events()).contains(&"shutdown"));
}

struct Unreadable;

impl Kernel for Unreadable {
    fn mces(&mut self, _: &BootId, _: Duration) -> Result<Vec<togi_machine::Mce>, Error> {
        Err(Error::Crashed)
    }

    fn read_mces(
        &mut self,
        _: &BootId,
        _: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<togi_machine::Mce>>> {
        Err(Partial {
            value: Vec::new(),
            error: Error::Crashed,
        })
    }

    fn reset_reason(&mut self, _: &BootId) -> Result<ResetReason, Error> {
        Err(Error::Crashed)
    }

    fn reset_reason_after(&mut self, _: &BootId) -> Result<ResetReason, Error> {
        Err(Error::Crashed)
    }

    fn saved_pstore(&mut self, _: &BootId) -> Result<Option<PstoreRecord>, Error> {
        Err(Error::Crashed)
    }
}

#[test]
fn without_the_proof_that_every_workload_stopped_nothing_is_restored_or_recorded() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    session.stopped = None;
    let before = kinds(session.appender().journal().events());
    let ended = close(session, run);
    assert_eq!(kinds(ended.journal.events()), before);
    assert_eq!(smu_offsets(&bench), [-5, -5]);
}

#[test]
fn a_machine_out_of_reach_is_left_alone_and_records_no_shutdown() {
    for fault in [Broken::Containment, Broken::Crashed] {
        let bench = Bench::new(2);
        let mut session = tuned(&bench);
        let run = Ok(signalled(&mut session));
        session.fault = Some(fault);
        let before = kinds(session.appender().journal().events());
        let ended = close(session, run);
        assert_eq!(kinds(ended.journal.events()), before, "{fault:?}");
        assert_eq!(smu_offsets(&bench), [-5, -5], "{fault:?}");
    }
}

#[test]
fn a_run_that_ended_in_a_crash_touches_nothing() {
    let bench = Bench::new(2);
    let session = tuned(&bench);
    let before = kinds(session.appender().journal().events());
    let ended = close(session, Err(Error::Crashed.into()));
    assert_eq!(kinds(ended.journal.events()), before);
    assert_eq!(smu_offsets(&bench), [-5, -5]);
    assert!(ended.errors[0].is_crash());
}

#[test]
fn a_crash_during_the_restoration_keeps_the_applied_condition_and_records_no_shutdown() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    bench.sim.crash_at_write(1);
    let ended = close(session, run);
    assert!(ended.errors.iter().any(SessionError::is_crash));
    let kinds = kinds(ended.journal.events());
    assert!(!kinds.contains(&"profile.restored"));
    assert!(!kinds.contains(&"shutdown"));
    assert_eq!(kinds.last(), Some(&"smu.intent"));
}

#[test]
fn a_restoration_that_fails_at_the_smu_turns_a_clean_stop_into_an_smu_dead_end() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    bench.sim.fail_write(1);
    let ended = close(session, run);
    assert!(ended.errors.is_empty());
    let Some(Stop::DeadEnd { dead_end, .. }) = &ended.stop else {
        panic!("{:?}", ended.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::Smu);
    let events = ended.journal.events();
    let kinds = kinds(events);
    let failed = kinds.iter().position(|&kind| kind == "smu.error").unwrap();
    assert!(!kinds[failed..].contains(&"smu.intent"));
    assert!(!kinds.contains(&"profile.restored"));
    assert_eq!(shutdown_reason(events), Some(ShutdownReason::DeadEnd));
    assert_eq!(smu_offsets(&bench), [-5, -5]);
}

#[test]
fn a_restoration_that_fails_at_the_smu_keeps_a_dead_end_the_run_already_reached() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let original = session
        .dead_end(dead_end(DeadEndCondition::NoEvidence), &[])
        .unwrap();
    bench.sim.fail_write(1);
    let ended = close(session, Ok(original.clone()));
    assert_eq!(ended.stop, Some(original));
    assert!(ended.errors.is_empty());
    let events = ended.journal.events();
    let kinds = kinds(events);
    assert!(kinds.contains(&"smu.error"));
    assert!(!kinds.contains(&"profile.restored"));
    assert_eq!(kinds.iter().filter(|&&kind| kind == "deadend").count(), 1);
    assert_eq!(shutdown_reason(events), Some(ShutdownReason::DeadEnd));
}

#[test]
fn an_smu_dead_end_leaves_the_machine_alone_and_still_records_its_shutdown() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let stop = session
        .dead_end(dead_end(DeadEndCondition::Smu), &[])
        .unwrap();
    let before = session.appender().journal().events().len();
    let ended = close(session, Ok(stop));
    let events = ended.journal.events();
    assert_eq!(kinds(&events[before..]), ["shutdown"]);
    assert_eq!(smu_offsets(&bench), [-5, -5]);
}

#[test]
fn a_boot_with_unreconciled_offsets_restores_but_records_no_shutdown() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    session.visit = Visit::Same { reconciled: false };
    let ended = close(session, run);
    let kinds = kinds(ended.journal.events());
    assert!(kinds.contains(&"profile.restored"));
    assert!(!kinds.contains(&"shutdown"));
    assert_eq!(smu_offsets(&bench), [0, 0]);
}

#[test]
fn drained_commitments_are_recorded_before_the_restoration() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    let warning = DecisionAction::new(
        togi_journal::TunerWarning {
            warning: togi_journal::WarningKind::Monotonicity,
            trial: None,
            passes: Vec::new(),
            detail: "drained".into(),
        },
        Vec::new(),
    );
    let ended = session.close(run, &held(), vec![warning]);
    let kinds = kinds(ended.journal.events());
    let drained = kinds
        .iter()
        .position(|&kind| kind == "tuner.warning")
        .unwrap();
    let restored = kinds
        .iter()
        .position(|&kind| kind == "profile.restored")
        .unwrap();
    assert!(drained < restored);
    assert_eq!(ended.stop, Some(Stop::Signal));
}

#[test]
fn a_drained_dead_end_replaces_the_stop_and_its_commitments_come_first() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    let drained = vec![DecisionAction::new(
        dead_end(DeadEndCondition::Preflight),
        Vec::new(),
    )];
    let ended = session.close(run, &held(), drained);
    let Some(Stop::DeadEnd { dead_end, .. }) = &ended.stop else {
        panic!("{:?}", ended.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::Preflight);
    let events = ended.journal.events();
    let kinds = kinds(events);
    let recorded = kinds.iter().position(|&kind| kind == "deadend").unwrap();
    let restored = kinds
        .iter()
        .position(|&kind| kind == "profile.restored")
        .unwrap();
    assert!(recorded < restored);
    assert_eq!(shutdown_reason(events), Some(ShutdownReason::DeadEnd));
}

#[test]
fn a_journal_failure_zeroes_the_machine_through_the_gate_and_appends_nothing() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    let before = kinds(session.appender().journal().events());
    bench.mem.inject(Fault::before(Op::Write, 1));
    let failure = session
        .appender
        .append(
            Payload::SessionWarning(SessionWarning {
                operation: "later".into(),
                trial: None,
                error: String::new(),
            }),
            &[],
        )
        .unwrap_err();
    let ended = close(session, run.and(Err(failure.into())));
    let Some(emergency) = ended.emergency else {
        panic!("no emergency");
    };
    assert!(matches!(&emergency.response, Response::Zeroed(zeroed) if zeroed.is_clean()));
    assert_eq!(kinds(ended.journal.events()), before);
    assert_eq!(smu_offsets(&bench), [0, 0]);
    assert!(matches!(ended.errors[0], SessionError::Journal(_)));
}

#[test]
fn an_event_the_run_cannot_fold_gets_the_emergency_like_a_failed_append() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    let error = session
        .appender
        .append(
            Payload::CorePhase(togi_journal::CoreTransition {
                core: core(9),
                from: None,
                to: togi_journal::CorePhase::Search,
                offset: target(-10),
                pass: None,
                failure_point: None,
                check_solo_limit: false,
                workloads: Vec::new(),
                cleared_combination: Vec::new(),
                reason: "test".into(),
            }),
            &[],
        )
        .unwrap_err();
    let ended = close(session, run.and(Err(error.into())));
    let Some(emergency) = ended.emergency else {
        panic!("no emergency");
    };
    assert!(matches!(&emergency.response, Response::Zeroed(zeroed) if zeroed.is_clean()));
    assert_eq!(smu_offsets(&bench), [0, 0]);
    assert!(matches!(ended.errors[0], SessionError::Fold(_)));
}

fn latched(bench: &Bench, prepare: impl FnOnce(&mut Session)) -> Ended {
    let mut session = tuned(bench);
    let run = Ok(signalled(&mut session));
    prepare(&mut session);
    bench.mem.inject(Fault::before(Op::Write, 1));
    drop(session.appender.append(
        Payload::SessionWarning(SessionWarning {
            operation: "later".into(),
            trial: None,
            error: String::new(),
        }),
        &[],
    ));
    close(session, run)
}

#[test]
fn the_emergency_is_withheld_without_a_validated_smu_or_after_a_fault() {
    let bench = Bench::new(2);
    let ended = latched(&bench, |session| session.validated = None);
    assert!(matches!(
        ended.emergency.unwrap().response,
        Response::Withheld(Withheld::NoValidation)
    ));
    assert_eq!(smu_offsets(&bench), [-5, -5]);

    let bench = Bench::new(2);
    let ended = latched(&bench, |session| session.stopped = None);
    assert!(matches!(
        ended.emergency.unwrap().response,
        Response::Withheld(Withheld::TeardownUnconfirmed)
    ));
    assert_eq!(smu_offsets(&bench), [-5, -5]);

    for (fault, reason) in [
        (Broken::Crashed, Withheld::Crashed),
        (Broken::Containment, Withheld::TeardownUnconfirmed),
    ] {
        let bench = Bench::new(2);
        let ended = latched(&bench, |session| session.fault = Some(fault));
        assert!(matches!(
            ended.emergency.unwrap().response,
            Response::Withheld(withheld) if withheld == reason
        ));
        assert_eq!(smu_offsets(&bench), [-5, -5]);
    }
}

#[test]
fn a_journal_that_fails_during_the_restoration_gets_the_emergency() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    bench.mem.inject(Fault::before(Op::Write, 2));
    let ended = close(session, run);
    assert!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, SessionError::Write(_) | SessionError::Journal(_)))
    );
    let Some(emergency) = ended.emergency else {
        panic!("no emergency");
    };
    assert!(matches!(emergency.response, Response::Zeroed(_)));
    assert_eq!(smu_offsets(&bench), [0, 0]);
}

struct Tail {
    inner: Box<dyn Kernel>,
    served: bool,
}

impl Kernel for Tail {
    fn mces(&mut self, boot: &BootId, since: Duration) -> Result<Vec<togi_machine::Mce>, Error> {
        self.inner.mces(boot, since)
    }

    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<togi_machine::Mce>>> {
        let mut read = self.inner.read_mces(boot, after)?;
        if !self.served {
            self.served = true;
            read.mces.push(togi_machine::Mce {
                cpu: CpuId(0),
                core: Some(core(0)),
                bank: 1,
                bank_type: BankType::LoadStore,
                corrected: true,
                time: Timestamp::UNIX_EPOCH,
                monotonic: Duration::ZERO,
                lines: vec!["restoration".into()],
            });
        }
        Ok(read)
    }

    fn reset_reason(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        self.inner.reset_reason(boot)
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        self.inner.reset_reason_after(boot)
    }

    fn saved_pstore(&mut self, boot: &BootId) -> Result<Option<PstoreRecord>, Error> {
        self.inner.saved_pstore(boot)
    }
}

#[test]
fn the_shutdown_follows_the_machine_check_of_the_restoration_tail() {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let stop = session
        .dead_end(dead_end(DeadEndCondition::Preflight), &[])
        .unwrap();
    let inner = std::mem::replace(&mut session.kernel, Box::new(Unreadable));
    session.kernel = Box::new(Tail {
        inner,
        served: false,
    });
    let ended = close(session, Ok(stop));
    let events = ended.journal.events();
    let [.., mce, shutdown] = events else {
        panic!("too few events");
    };
    let Payload::Mce(mce) = &mce.payload else {
        panic!("{:?}", mce.payload);
    };
    assert!(mce.between_trials && mce.trial.is_none());
    assert_eq!(mce.lines, ["restoration"]);
    let Payload::Shutdown(shutdown) = &shutdown.payload else {
        panic!("{:?}", shutdown.payload);
    };
    assert!(shutdown.kernel_cursor.is_some());
}

#[test]
fn a_decision_that_cannot_be_folded_gets_the_emergency_instead_of_the_restoration_and_no_shutdown()
{
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    let unfoldable = DecisionAction::new(
        togi_journal::CoreTransition {
            core: core(9),
            from: None,
            to: togi_journal::CorePhase::AtLimit,
            offset: target(-6),
            pass: None,
            failure_point: None,
            check_solo_limit: false,
            workloads: Vec::new(),
            cleared_combination: Vec::new(),
            reason: "test".into(),
        },
        Vec::new(),
    );
    let ended = session.close(run, &held(), vec![unfoldable]);
    assert!(matches!(ended.errors.first(), Some(SessionError::Fold(_))));
    let kinds = kinds(ended.journal.events());
    assert!(kinds.contains(&"smu.intent"));
    assert!(!kinds.contains(&"profile.restored"));
    assert!(!kinds.contains(&"shutdown"));
    let Some(emergency) = ended.emergency else {
        panic!("no emergency");
    };
    assert!(matches!(&emergency.response, Response::Zeroed(zeroed) if zeroed.is_clean()));
    assert_eq!(smu_offsets(&bench), [0, 0]);
}
