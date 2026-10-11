use std::cell::Cell;
use std::rc::Rc;

use togi_journal::fs::{Fault as FsFault, Op};
use togi_journal::{DeadEndCondition, Payload, SessionContext};
use togi_machine::{CancellationToken, Error};

use super::*;
use crate::emergency::Response;
use crate::session::{ConfigSource, Stop};
use crate::testing::{
    Bench, MemBoot, SweepFails, ValidationFails, kinds_of, offsets, put, recorded_baseline, swept,
};

fn started(bench: &Bench) -> Session {
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    session
}

fn checks(session: &Session) -> Vec<(String, bool)> {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::PreflightCheck(check) => Some((check.check.clone(), check.ok)),
            _ => None,
        })
        .collect()
}

fn dead_end_of(control: Control) -> (togi_journal::DeadEnd, Vec<Seq>) {
    let Control::Stop(Stop::DeadEnd {
        dead_end, causes, ..
    }) = control
    else {
        panic!("{control:?}");
    };
    (dead_end, causes)
}

fn record_context(session: &mut Session) {
    let context = session.host.bios_context().unwrap();
    let payload = SessionContext {
        bios_version: context.bios_version,
        board: context.board,
        cpu_model: context.cpu_model,
        microcode: context.microcode,
        boost_limit_mhz: context.boost_limit_mhz,
    };
    session
        .appender
        .append(Payload::SessionContext(payload), &[])
        .unwrap();
}

#[test]
fn preflight_records_every_check_and_proves_the_smu() {
    let bench = Bench::new(2);
    let mut session = started(&bench);
    assert_eq!(session.preflight().unwrap(), Control::Continue);
    let recorded = checks(&session);
    assert_eq!(recorded.len(), 8);
    assert!(recorded.iter().all(|(_, ok)| *ok));
    assert!(session.validated.is_some());
    crate::testing::assert_fold_consistent(session.appender());
}

#[test]
fn failed_checks_are_a_preflight_dead_end_citing_them_and_skip_the_sweep() {
    let bench = Bench::new(2);
    bench.sim.fail_check("root", "not root");
    bench.sim.fail_check("backends", "no mprime");
    let mut session = started(&bench);
    let (dead_end, causes) = dead_end_of(session.preflight().unwrap());
    assert_eq!(dead_end.condition, DeadEndCondition::Preflight);
    assert_eq!(
        dead_end.detail,
        "failed checks: root (not root), backends (no mprime)"
    );
    assert_eq!(causes.len(), 2);
    assert!(session.sweep.is_none());
    assert_eq!(kinds_of(session.appender()).last(), Some(&"deadend"));
}

#[test]
fn a_changed_bios_context_is_a_preflight_dead_end() {
    let bench = Bench::new(2);
    let mut session = started(&bench);
    record_context(&mut session);
    let mut changed = session.host.bios_context().unwrap();
    changed.microcode = "0xdeadbeef".to_owned();
    bench.sim.set_bios_context(changed);
    let (dead_end, causes) = dead_end_of(session.preflight().unwrap());
    assert!(
        dead_end
            .detail
            .starts_with("failed checks: bios_context (microcode is 0xdeadbeef")
    );
    assert_eq!(causes.len(), 1);
    assert_eq!(
        checks(&session).last().unwrap(),
        &("bios_context".to_owned(), false)
    );
}

#[test]
fn the_bios_context_is_not_read_after_a_failed_check() {
    let bench = Bench::new(2);
    bench.sim.fail_check("cpu", "wrong cpu");
    let mut session = started(&bench);
    record_context(&mut session);
    let (dead_end, _) = dead_end_of(session.preflight().unwrap());
    assert!(!dead_end.detail.contains("bios_context"));
    assert!(
        checks(&session)
            .iter()
            .all(|(name, _)| name != "bios_context")
    );
}

#[test]
fn a_matching_bios_context_is_recorded_as_passing() {
    let bench = Bench::new(2);
    let mut session = started(&bench);
    record_context(&mut session);
    assert_eq!(session.preflight().unwrap(), Control::Continue);
    assert_eq!(
        checks(&session).last().unwrap(),
        &("bios_context".to_owned(), true)
    );
}

#[test]
fn the_sweep_hands_over_the_proof_that_nothing_runs() {
    let bench = Bench::new(2);
    let mut session = started(&bench);
    session.preflight().unwrap();
    assert_eq!(session.sweep().unwrap(), Control::Continue);
    assert!(session.sweep.is_some());
    assert!(session.stopped.is_some());
    assert_eq!(
        checks(&session).last().unwrap(),
        &("trial_scopes".to_owned(), true)
    );
}

#[test]
fn a_failed_sweep_is_a_containment_dead_end_and_leaves_the_smu_alone() {
    let bench = Bench::new(2);
    let mut input = bench.input();
    let trials = input.machine.trials;
    input.machine.trials = Box::new(SweepFails(trials, || Error::Other("scope busy".into())));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    session.start().unwrap();
    session.preflight().unwrap();
    let (dead_end, causes) = dead_end_of(session.sweep().unwrap());
    assert_eq!(dead_end.condition, DeadEndCondition::Containment);
    assert_eq!(dead_end.detail, "scope busy");
    assert_eq!(causes.len(), 1);
    assert!(session.sweep.is_none());
    assert!(session.stopped.is_none());
    assert_eq!(
        checks(&session).last().unwrap(),
        &("trial_scopes".to_owned(), false)
    );
    assert!(
        !kinds_of(session.appender())
            .iter()
            .any(|kind| kind.starts_with("smu."))
    );
}

#[test]
fn a_sweep_whose_check_cannot_be_recorded_still_hands_the_emergency_its_proofs() {
    let bench = Bench::new(2);
    drop(left_applied(&bench, &[-10, -12]));
    let mut session = bench.open();
    session.start().unwrap();
    session.preflight().unwrap();
    bench.mem.inject(FsFault::before(Op::Write, 1));
    let error = session.sweep().unwrap_err();
    assert!(matches!(error, SessionError::Journal(_)));
    let ended = session.close(Err(error));
    let Some(emergency) = ended.emergency else {
        panic!("no emergency");
    };
    assert!(matches!(&emergency.response, Response::Zeroed(zeroed) if zeroed.is_clean()));
    let mut smu = bench.sim.seams().smu;
    let machine: Vec<i64> = (0..2)
        .map(|n| smu.offset(crate::testing::core(n)).unwrap().0)
        .collect();
    assert_eq!(machine, [0, 0]);
}

#[test]
fn an_unconfirmed_sweep_latches_the_containment_before_its_check_is_recorded() {
    let bench = Bench::new(2);
    let mut input = bench.input();
    let trials = input.machine.trials;
    input.machine.trials = Box::new(SweepFails(trials, || Error::Other("scope busy".into())));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    session.start().unwrap();
    session.preflight().unwrap();
    bench.mem.inject(FsFault::before(Op::Write, 1));
    assert!(session.sweep().is_err());
    assert_eq!(session.fault, Some(crate::session::Fault::Containment));
}

#[test]
fn an_unconfirmed_sweep_never_ends_in_a_shutdown_or_touches_the_smu() {
    let bench = Bench::new(2);
    let accesses = Rc::new(Cell::new(0_u32));
    let counter = Rc::clone(&accesses);
    bench
        .sim
        .interrupt_smu(move |_: &togi_sim::faults::SmuOperation| {
            counter.set(counter.get() + 1);
            Ok(())
        });
    let mut input = bench.input();
    let trials = input.machine.trials;
    input.machine.trials = Box::new(SweepFails(trials, || Error::Other("scope busy".into())));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    session.start().unwrap();
    session.preflight().unwrap();
    let Control::Stop(stop) = session.sweep().unwrap() else {
        panic!("no stop");
    };
    let ended = session.close(Ok(stop));
    assert!(ended.errors.is_empty());
    assert!(
        !ended
            .journal
            .events()
            .iter()
            .any(|event| event.kind().as_str() == "shutdown")
    );
    assert_eq!(accesses.get(), 0);
}

#[test]
fn a_machine_that_crashes_in_the_sweep_is_an_error_not_a_dead_end() {
    let bench = Bench::new(2);
    let mut input = bench.input();
    let trials = input.machine.trials;
    input.machine.trials = Box::new(SweepFails(trials, || Error::Crashed));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    session.start().unwrap();
    assert!(matches!(
        session.sweep(),
        Err(SessionError::Machine(Error::Crashed))
    ));
}

fn started_with_failing_validation(bench: &Bench, error: fn() -> Error) -> Session {
    let mut input = bench.input();
    let host = input.machine.host;
    input.machine.host = Box::new(ValidationFails(host, error));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    assert_eq!(session.start().unwrap(), Control::Continue);
    session
}

#[test]
fn a_machine_that_crashes_while_the_smu_is_validated_is_an_error() {
    let bench = Bench::new(2);
    let mut session = started_with_failing_validation(&bench, || Error::Crashed);
    let before = kinds_of(session.appender());
    assert!(matches!(
        session.preflight(),
        Err(SessionError::Machine(Error::Crashed))
    ));
    assert_eq!(kinds_of(session.appender()), before);
}

#[test]
fn an_smu_that_cannot_be_validated_is_a_warning_and_leaves_the_emergency_withheld() {
    let bench = Bench::new(2);
    let mut session =
        started_with_failing_validation(&bench, || Error::Other("no SMU driver".into()));
    assert_eq!(session.preflight().unwrap(), Control::Continue);
    assert!(session.validated.is_none());
    let warning = session
        .appender()
        .journal()
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::SessionWarning(warning) => Some(warning.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(warning.operation, "validate the SMU interface");
    assert!(warning.error.contains("no SMU driver"));
}

fn from_the_tuning_boot(bench: &Bench, cancel: CancellationToken) -> Session {
    let mut input = bench.input();
    input.bootloader = Some(Box::new(MemBoot::default()));
    input.cancel = cancel;
    input.source = ConfigSource::default();
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    assert_eq!(session.start().unwrap(), Control::Continue);
    session
}

#[test]
fn a_watchdog_that_never_arms_is_a_preflight_dead_end_after_thirty_seconds() {
    let bench = Bench::new(2);
    bench.sim.fail_check("watchdog", "no /dev/watchdog");
    let before = bench.sim.monotonic();
    let mut session = from_the_tuning_boot(&bench, CancellationToken::new());
    let (dead_end, causes) = dead_end_of(session.preflight().unwrap());
    assert_eq!(dead_end.condition, DeadEndCondition::Preflight);
    assert_eq!(
        dead_end.detail,
        "failed checks: watchdog (no /dev/watchdog; waited 30s for an armed hardware watchdog)"
    );
    assert_eq!(causes.len(), 1);
    assert!(bench.sim.monotonic().saturating_sub(before) >= WATCHDOG_WAIT);
    assert_eq!(checks(&session), [("watchdog".to_owned(), false)]);
}

#[test]
fn an_armed_watchdog_is_recorded_without_waiting() {
    let bench = Bench::new(2);
    let before = bench.sim.monotonic();
    let mut session = from_the_tuning_boot(&bench, CancellationToken::new());
    assert_eq!(session.preflight().unwrap(), Control::Continue);
    assert_eq!(checks(&session)[0], ("watchdog".to_owned(), true));
    assert!(bench.sim.monotonic().saturating_sub(before) < WATCHDOG_STEP);
}

#[test]
fn a_signal_while_waiting_for_the_watchdog_stops_the_run() {
    let bench = Bench::new(2);
    bench.sim.fail_check("watchdog", "no /dev/watchdog");
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut session = from_the_tuning_boot(&bench, cancel);
    let control = session.preflight().unwrap();
    assert!(matches!(control, Control::Stop(Stop::Signal)));
    assert!(checks(&session).is_empty());
}

fn left_applied(bench: &Bench, values: &[i64]) -> Session {
    let mut session = swept(bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, values);
    session
}

fn resumed(bench: &Bench) -> Session {
    let mut session = bench.open();
    assert!(session.unreconciled());
    assert_eq!(session.start().unwrap(), Control::Continue);
    assert_eq!(session.sweep().unwrap(), Control::Continue);
    session
}

fn machine_offsets(session: &mut Session) -> Vec<i64> {
    (0..2)
        .map(|n| session.smu.offset(crate::testing::core(n)).unwrap().0)
        .collect()
}

#[test]
fn a_resumed_boot_reads_what_the_earlier_run_left_and_puts_the_baseline_back() {
    let bench = Bench::new(2);
    drop(left_applied(&bench, &[-10, -12]));
    let mut session = resumed(&bench);
    let before = kinds_of(session.appender()).len();
    assert_eq!(
        session.reconcile(&offsets(&[0, 0])).unwrap(),
        Control::Continue
    );
    assert!(!session.unreconciled());
    assert_eq!(machine_offsets(&mut session), [0, 0]);
    let kinds = kinds_of(session.appender());
    assert_eq!(kinds[before..before + 2], ["smu.readback", "smu.readback"]);
    assert_eq!(kinds.last(), Some(&"profile.restored"));
    crate::testing::assert_fold_consistent(session.appender());
}

#[test]
fn a_boot_that_left_no_baseline_has_nothing_to_reconcile() {
    let bench = Bench::new(2);
    let mut first = bench.open();
    first.start().unwrap();
    drop(first);
    let mut session = bench.open();
    session.start().unwrap();
    let before = kinds_of(session.appender());
    assert_eq!(
        session.reconcile(&offsets(&[0, 0])).unwrap(),
        Control::Continue
    );
    assert_eq!(kinds_of(session.appender()), before);
}

#[test]
fn a_read_that_fails_while_reconciling_is_an_smu_dead_end_and_the_boot_stays_unreconciled() {
    let bench = Bench::new(2);
    drop(left_applied(&bench, &[-10, -12]));
    let mut session = resumed(&bench);
    bench
        .sim
        .interrupt_smu(|operation: &togi_sim::faults::SmuOperation| {
            if operation.op == togi_sim::faults::SmuOp::Read {
                return Err(Error::Other("mailbox timeout".into()));
            }
            Ok(())
        });
    let control = session.reconcile(&offsets(&[0, 0])).unwrap();
    let (dead_end, _) = dead_end_of(control);
    assert_eq!(dead_end.condition, DeadEndCondition::Smu);
    assert!(session.unreconciled());
    assert!(kinds_of(session.appender()).contains(&"smu.error"));
}

#[test]
fn a_dead_end_pending_on_the_boot_is_finished_once_the_boot_is_reconciled() {
    let bench = Bench::new(2);
    let mut first = left_applied(&bench, &[-10, -12]);
    let pending = DeadEnd {
        condition: DeadEndCondition::Smu,
        core: None,
        detail: "smu readback disagreed".to_owned(),
        action: Some(togi_journal::DeadEndAction::Exit),
    };
    first
        .appender
        .append(Payload::DeadEnd(pending), &[])
        .unwrap();
    drop(first);
    let mut session = resumed(&bench);
    assert_eq!(
        session.reconcile(&offsets(&[0, 0])).unwrap(),
        Control::Continue
    );
    assert_eq!(machine_offsets(&mut session), [0, 0]);
    let (dead_end, _) = dead_end_of(session.inspect().unwrap());
    assert_eq!(dead_end.detail, "smu readback disagreed");
    assert!(session.shutdown.is_some());
}

#[test]
fn a_stop_asked_for_while_the_earlier_boots_were_read_waits_for_the_reconciliation() {
    let bench = Bench::new(2);
    drop(left_applied(&bench, &[-10, -12]));
    let mut session = resumed(&bench);
    session.recovery_cancelled = true;
    session.reconcile(&offsets(&[0, 0])).unwrap();
    assert_eq!(machine_offsets(&mut session), [0, 0]);
    assert!(matches!(
        session.inspect().unwrap(),
        Control::Stop(Stop::Signal)
    ));
}

#[test]
fn a_resumed_boot_does_not_give_up_on_the_watchdog_before_it_is_reconciled() {
    let bench = Bench::new(2);
    drop(left_applied(&bench, &[-10, -12]));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut input = bench.input();
    input.bootloader = Some(Box::new(MemBoot::default()));
    input.cancel = cancel;
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    session.start().unwrap();
    assert_eq!(session.preflight().unwrap(), Control::Continue);
    assert!(checks(&session).contains(&("watchdog".to_owned(), true)));
    assert_eq!(session.sweep().unwrap(), Control::Continue);
}

fn crash_records(session: &Session) -> usize {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter(|event| matches!(event.payload, Payload::CrashDetected(_)))
        .count()
}

#[test]
fn a_boot_that_crashed_before_this_one_gets_its_crash_recorded() {
    let bench = Bench::new(2);
    let mut earlier = bench.open();
    earlier.start().unwrap();
    drop(earlier);
    bench.sim.crash();
    bench.sim.reboot();
    let mut session = bench.open();
    session.start().unwrap();
    assert_eq!(crash_records(&session), 0);
    assert_eq!(session.recover_prior().unwrap(), Control::Continue);
    assert_eq!(crash_records(&session), 1);
    crate::testing::assert_fold_consistent(session.appender());
}

#[test]
fn a_dead_end_pending_from_another_boot_is_not_recovered_around() {
    let bench = Bench::new(2);
    let mut earlier = bench.open();
    earlier.start().unwrap();
    earlier
        .appender
        .append(
            Payload::DeadEnd(togi_journal::DeadEnd {
                condition: DeadEndCondition::Preflight,
                detail: "failed".into(),
                action: Some(togi_journal::DeadEndAction::Exit),
                core: None,
            }),
            &[],
        )
        .unwrap();
    drop(earlier);
    bench.sim.crash();
    bench.sim.reboot();
    let mut session = bench.open();
    let before = kinds_of(session.appender());
    assert_eq!(session.recover_prior().unwrap(), Control::Continue);
    assert_eq!(kinds_of(session.appender()), before);
}

#[test]
fn an_unreadable_kernel_log_is_no_evidence_citing_the_retries() {
    let bench = Bench::new(2);
    let mut session = bench.open();
    session.start().unwrap();
    assert!(session.check_dead_end().unwrap().is_none());
    let retry = session.appender().journal().events()[0].seq;
    session.unreadable = Some(crate::recover::KernelUnreadable {
        detail: "kernel log unreadable".into(),
        retries: vec![retry],
    });
    let (dead_end, causes) = dead_end_of(Control::Stop(session.check_dead_end().unwrap().unwrap()));
    assert_eq!(dead_end.condition, DeadEndCondition::NoEvidence);
    assert_eq!(dead_end.detail, "kernel log unreadable");
    assert_eq!(causes, [retry]);
}

#[test]
fn the_same_boot_trial_waits_for_the_boot_to_be_swept() {
    let bench = Bench::new(2);
    let mut first = bench.open();
    first.start().unwrap();
    assert_eq!(first.recover_same_boot().unwrap(), Control::Continue);
    drop(first);
    let mut again = bench.open();
    assert!(again.same_boot());
    again.start().unwrap();
    let before = kinds_of(again.appender());
    assert_eq!(again.recover_same_boot().unwrap(), Control::Continue);
    assert_eq!(kinds_of(again.appender()), before);
}
