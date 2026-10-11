use togi_journal::{DeadEndCondition, Payload, SessionContext};
use togi_machine::{CancellationToken, Error};

use super::*;
use crate::session::{ConfigSource, Stop};
use crate::testing::{Bench, MemBoot, SweepFails, kinds_of};

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
    let Control::Stop(Stop {
        reason: StopReason::DeadEnd,
        dead_end: Some(found),
        ..
    }) = control
    else {
        panic!("{control:?}");
    };
    found
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
    assert!(matches!(
        control,
        Control::Stop(Stop {
            reason: StopReason::Signal,
            ..
        })
    ));
    assert!(checks(&session).is_empty());
}
