use std::time::Duration;
use test_case::test_case;

use jiff::Timestamp;
use std::collections::BTreeMap;
use togi_journal::fs::{Fault, Op};
use togi_journal::{DeadEnd, DeadEndCondition, Event, Payload, SessionWarning, ShutdownReason};

use togi_machine::{
    BankType, BootId, CpuId, Error, Kernel, KernelCursor, KernelRead, Partial, PstoreRecord,
    Regime, ResetReason, Signal,
};
use togi_sim::MachineDefinition;
use togi_sim::definition::{CoreDef, JointDef, ModelDef, Weights};

use crate::emergency::{Response, Withheld};
use crate::session::Session;
use crate::session::{Fault as Broken, Lifecycle, Visit};
use crate::testing::{Bench, core, put, recorded_baseline, swept, target};
use crate::{Ended, SessionError, Stop};

fn tuned(bench: &Bench) -> Session<'static> {
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

fn close(session: Session, run: Result<Stop, SessionError>) -> Ended {
    session.close(run)
}

fn failed_search(bench: &Bench, offset: i64) -> Session<'static> {
    failed_search_on(swept(bench), bench, offset)
}

fn failed_search_on(mut session: Session<'static>, bench: &Bench, offset: i64) -> Session<'static> {
    session.config = togi_config::Config::parse(&format!(
        r#"{{"start_offsets":{{"0":{offset},"1":{offset}}}}}"#
    ))
    .unwrap();
    session.begin().unwrap();
    session.validated = Some(session.host.validate_smu().unwrap());
    bench
        .sim
        .script_trial(
            "0001",
            togi_sim::model::Script {
                signal: Some(togi_machine::Signal::ComputationError),
                at_s: 1.0,
                core: core(0),
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
    let plan = crate::testing::search_plan(&session, core(0), togi_machine::Regime::R1);
    crate::testing::run_plan(&mut session, plan).unwrap();
    assert!(session.appender.tuner().drain().is_some());
    session
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
    if let Payload::Shutdown(shutdown) = &events.last()?.payload {
        Some(shutdown.reason)
    } else {
        None
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
    fn mces(
        &mut self,
        _: &BootId,
        _: Duration,
    ) -> Result<Vec<togi_machine::Mce>, Partial<Vec<togi_machine::Mce>>> {
        Err(Partial {
            value: Vec::new(),
            error: Error::Crashed,
        })
    }

    fn read_mces(
        &mut self,
        _: &BootId,
        _: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<KernelRead>> {
        Err(Partial {
            value: KernelRead::default(),
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
    session.lifecycle.lose_stopped();
    let before = kinds(session.appender().journal().events());
    let ended = close(session, run);
    assert_eq!(kinds(ended.journal.events()), before);
    assert_eq!(smu_offsets(&bench), [-5, -5]);
}

#[test_case(Broken::Containment; "Broken::Containment")]
#[test_case(Broken::Crashed; "Broken::Crashed")]
fn a_machine_out_of_reach_is_left_alone_and_records_no_shutdown(fault: Broken) {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    session.lifecycle = Lifecycle::Faulted(fault);
    let before = kinds(session.appender().journal().events());
    let ended = close(session, run);
    assert_eq!(kinds(ended.journal.events()), before, "{fault:?}");
    assert_eq!(smu_offsets(&bench), [-5, -5], "{fault:?}");
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

/// The dead end the restoration recorded is the run's stop even when a panic strikes the shutdown after it.
#[test]
fn a_panic_taking_in_the_shutdown_keeps_the_dead_end_the_restoration_recorded() {
    let bench = Bench::new(2);
    let armed = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = armed.clone();
    let mut input = bench.input();
    input.on_append = Some(Box::new(move |event| {
        assert!(
            !(flag.get() && event.kind() == togi_journal::EventKind::Shutdown),
            "a bug taking in the shutdown"
        );
    }));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    assert_eq!(session.start().unwrap(), crate::session::Control::Continue);
    assert_eq!(session.sweep().unwrap(), crate::session::Control::Continue);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-5, -5]);
    session.validated = Some(session.host.validate_smu().unwrap());
    let run = Ok(signalled(&mut session));
    bench.sim.fail_write(1);
    armed.set(true);
    let mut ended = session.close(run);
    let panic = ended.panic.take().expect("the panic is handed back");
    assert_eq!(panic.message(), "a bug taking in the shutdown");
    let Some(Stop::DeadEnd { dead_end, .. }) = &ended.stop else {
        panic!("{:?}", ended.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::Smu);
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::Panicked(message)] if message == "a bug taking in the shutdown"
    ));
    let kinds = kinds(ended.journal.events());
    assert!(
        kinds.ends_with(&["smu.error", "deadend", "shutdown"]),
        "{kinds:?}"
    );
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
    let mut session = failed_search(&bench, -5);
    let run = Ok(signalled(&mut session));
    let ended = session.close(run);
    let kinds = kinds(ended.journal.events());
    let drained = kinds.iter().position(|&kind| kind == "failure").unwrap();
    let restored = kinds.iter().position(|&kind| kind == "shutdown").unwrap();
    assert!(drained < restored);
    assert_eq!(ended.stop, Some(Stop::Signal));
}

/// A bootloader whose saved entry cannot be cleared because the code clearing it panics.
#[derive(Default)]
struct PanicsClearing(crate::testing::MemBoot);

impl togi_tuningboot::Environment for PanicsClearing {
    fn get(&mut self, name: &str) -> std::io::Result<Option<String>> {
        self.0.get(name)
    }

    fn set(&mut self, values: &[(&str, &str)]) -> std::io::Result<()> {
        self.0.set(values)
    }

    fn unset(&mut self, names: &[&str]) -> std::io::Result<()> {
        self.0.unset(names)
    }
}

impl togi_tuningboot::Bootloader for PanicsClearing {
    fn clear_saved_entry(&mut self) -> std::io::Result<togi_tuningboot::SavedEntry> {
        panic!("a bug clearing the saved entry");
    }
}

/// A panic during the one step of a clean stop the run's own catch does not cover.
#[test]
fn a_panic_draining_a_signalled_stop_still_restores_the_offsets_with_readback_and_is_handed_back() {
    let bench = Bench::new(2);
    let mut input = bench.input();
    input.bootloader = Some(Box::new(PanicsClearing::default()));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    assert_eq!(session.start().unwrap(), crate::session::Control::Continue);
    assert_eq!(session.sweep().unwrap(), crate::session::Control::Continue);
    let mut session = failed_search_on(session, &bench, 0);
    put(&mut session, &[-5, -5]);
    let run = Ok(signalled(&mut session));
    let mut ended = session.close(run);
    let panic = ended.panic.take().expect("the panic is handed back");
    assert_eq!(panic.message(), "a bug clearing the saved entry");
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::Panicked(message)] if message == "a bug clearing the saved entry"
    ));
    let kinds = kinds(ended.journal.events());
    assert!(
        kinds.contains(&"deadend"),
        "the drain got as far as the dead end"
    );
    assert!(kinds.contains(&"profile.restored"), "{kinds:?}");
    assert!(!kinds.contains(&"shutdown"), "no clean stop is claimed");
    assert_eq!(smu_offsets(&bench), [0, 0]);
    let readbacks = kinds.iter().filter(|&&kind| kind == "smu.readback").count();
    assert!(readbacks > 0, "the restoration was read back");
}

#[test]
fn a_drained_dead_end_replaces_the_stop_and_its_commitments_come_first() {
    let bench = Bench::new(2);
    let mut session = failed_search(&bench, 0);
    let run = Ok(signalled(&mut session));
    let ended = session.close(run);
    let Some(Stop::DeadEnd { dead_end, .. }) = &ended.stop else {
        panic!("{:?}", ended.stop);
    };
    assert_eq!(dead_end.condition, DeadEndCondition::FailureAtZero);
    let events = ended.journal.events();
    let kinds = kinds(events);
    let recorded = kinds.iter().position(|&kind| kind == "deadend").unwrap();
    let restored = kinds.iter().position(|&kind| kind == "shutdown").unwrap();
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

#[test_case(Broken::Crashed, Withheld::Crashed; "Broken::Crashed, Withheld::Crashed")]
#[test_case(Broken::Containment, Withheld::TeardownUnconfirmed; "Broken::Containment, Withheld::TeardownUnconfirmed")]
fn the_emergency_is_withheld_without_a_validated_smu_or_after_a_fault(
    fault: Broken,
    reason: Withheld,
) {
    let bench = Bench::new(2);
    let ended = latched(&bench, |session| session.validated = None);
    assert!(matches!(
        ended.emergency.unwrap().response,
        Response::Withheld(Withheld::NoValidation)
    ));
    assert_eq!(smu_offsets(&bench), [-5, -5]);

    let bench = Bench::new(2);
    let ended = latched(&bench, |session| session.lifecycle.lose_stopped());
    assert!(matches!(
        ended.emergency.unwrap().response,
        Response::Withheld(Withheld::TeardownUnconfirmed)
    ));
    assert_eq!(smu_offsets(&bench), [-5, -5]);

    let bench = Bench::new(2);
    let ended = latched(&bench, |session| {
        session.lifecycle = Lifecycle::Faulted(fault);
    });
    assert!(matches!(
        ended.emergency.unwrap().response,
        Response::Withheld(withheld) if withheld == reason
    ));
    assert_eq!(smu_offsets(&bench), [-5, -5]);
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
    fn mces(
        &mut self,
        boot: &BootId,
        since: Duration,
    ) -> Result<Vec<togi_machine::Mce>, Partial<Vec<togi_machine::Mce>>> {
        self.inner.mces(boot, since)
    }

    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<KernelRead>> {
        let mut read = self.inner.read_mces(boot, after)?;
        if !self.served {
            self.served = true;
            read.mces.push(togi_machine::Mce {
                cpu: CpuId::from(0),
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
    let unfoldable = togi_journal::CoreTransition {
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
    };
    let error = session
        .appender
        .append(Payload::CorePhase(unfoldable), &[])
        .unwrap_err();
    let ended = session.close(Err(error.into()));
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

/// A session that recorded no baseline, so its cleanup writes nothing to the SMU and appends only the `shutdown` the
/// stop owes.
fn settled(bench: &Bench) -> Session<'static> {
    let mut session = swept(bench);
    put(&mut session, &[-5, -5]);
    session.validated = Some(session.host.validate_smu().unwrap());
    session
}

fn fail_events_write(bench: &Bench, nth: u64) {
    bench
        .mem
        .inject(Fault::before(Op::Write, nth).named("events.jsonl"));
}

#[test]
fn a_crash_at_the_final_kernel_boundary_leaves_the_machine_alone_when_publication_and_its_warning_fail()
 {
    let bench = Bench::new(2);
    let mut session = settled(&bench);
    let run = Ok(signalled(&mut session));
    session.kernel = Box::new(Unreadable);
    let before = kinds(session.appender().journal().events());
    bench.mem.fail_state_publication();
    fail_events_write(&bench, 1);
    let ended = close(session, run);
    assert!(
        matches!(ended.errors.as_slice(), [error] if error.is_crash()),
        "{:?}",
        ended.errors
    );
    assert!(ended.emergency.is_none(), "{:?}", ended.emergency);
    assert_eq!(kinds(ended.journal.events()), before);
    assert_eq!(smu_offsets(&bench), [-5, -5]);
}

#[test_case(1; "1")]
#[test_case(2; "2")]
fn an_append_failing_while_a_signal_drains_keeps_its_error_skips_the_restoration_and_gets_the_emergency(
    nth: u64,
) {
    let bench = Bench::new(2);
    let mut session = failed_search(&bench, -5);
    put(&mut session, &[-5, -5]);
    let run = Ok(signalled(&mut session));
    let before = session.appender().journal().events().len();
    fail_events_write(&bench, nth);
    let ended = close(session, run);
    assert!(
        matches!(ended.errors.as_slice(), [SessionError::Journal(_)]),
        "nth {nth}: {:?}",
        ended.errors
    );
    let events = ended.journal.events();
    let drained = &events[before..];
    assert_eq!(
        drained.len(),
        usize::try_from(nth).unwrap() - 1,
        "nth {nth}: {:?}",
        kinds(drained)
    );
    let kinds = kinds(events);
    assert!(!kinds.contains(&"profile.restored"), "nth {nth}");
    assert!(!kinds.contains(&"shutdown"), "nth {nth}");
    let Some(emergency) = ended.emergency else {
        panic!("nth {nth}: no emergency");
    };
    assert!(
        matches!(&emergency.response, Response::Zeroed(zeroed) if zeroed.is_clean()),
        "nth {nth}"
    );
    assert_eq!(smu_offsets(&bench), [0, 0], "nth {nth}");
    assert!(ended.capsule.is_none(), "nth {nth}");
}

#[test]
fn an_append_failing_while_a_signal_drains_withholds_the_emergency_without_a_validated_smu() {
    let bench = Bench::new(2);
    let mut session = failed_search(&bench, -5);
    put(&mut session, &[-5, -5]);
    let run = Ok(signalled(&mut session));
    session.validated = None;
    fail_events_write(&bench, 1);
    let ended = close(session, run);
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::Journal(_)]
    ));
    assert!(matches!(
        ended.emergency.unwrap().response,
        Response::Withheld(Withheld::NoValidation)
    ));
    assert_eq!(smu_offsets(&bench), [-5, -5]);
    assert!(!kinds(ended.journal.events()).contains(&"shutdown"));
}

#[test]
fn a_final_publication_and_its_warning_both_failing_ends_the_run_with_the_emergency_and_no_capsule()
{
    let bench = Bench::new(2);
    let mut session = settled(&bench);
    let run = Ok(signalled(&mut session));
    let before = session.appender().journal().events().len();
    bench.mem.fail_state_publication();
    fail_events_write(&bench, 2);
    let ended = close(session, run);
    assert_eq!(ended.stop, Some(Stop::Signal));
    assert!(
        matches!(ended.errors.as_slice(), [SessionError::Journal(_)]),
        "{:?}",
        ended.errors
    );
    let events = ended.journal.events();
    assert_eq!(kinds(&events[before..]), ["shutdown"]);
    let Some(emergency) = ended.emergency else {
        panic!("no emergency");
    };
    assert!(matches!(&emergency.response, Response::Zeroed(zeroed) if zeroed.is_clean()));
    assert_eq!(smu_offsets(&bench), [0, 0]);
    assert!(ended.capsule.is_none());
}

#[test]
fn a_final_publication_and_its_warning_both_failing_withholds_the_emergency_without_a_validated_smu()
 {
    let bench = Bench::new(2);
    let mut session = settled(&bench);
    let run = Ok(signalled(&mut session));
    session.validated = None;
    let before = session.appender().journal().events().len();
    bench.mem.fail_state_publication();
    fail_events_write(&bench, 2);
    let ended = close(session, run);
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::Journal(_)]
    ));
    assert_eq!(kinds(&ended.journal.events()[before..]), ["shutdown"]);
    assert!(matches!(
        ended.emergency.unwrap().response,
        Response::Withheld(Withheld::NoValidation)
    ));
    assert_eq!(smu_offsets(&bench), [-5, -5]);
    assert!(ended.capsule.is_none());
}

/// Runs the restoration that fails at the SMU on `bench` and returns how many events it appended up to and
/// including the `deadend` that records it.
fn appends_before_the_restoration_dead_end() -> usize {
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    let before = session.appender().journal().events().len();
    bench.sim.fail_write(1);
    let ended = close(session, run);
    let events = ended.journal.events();
    events[before..]
        .iter()
        .position(|event| matches!(event.payload, Payload::DeadEnd(_)))
        .unwrap()
        + 1
}

#[test]
fn a_dead_end_a_failed_restoration_cannot_record_keeps_that_error_records_no_shutdown_and_gets_the_emergency()
 {
    let nth = u64::try_from(appends_before_the_restoration_dead_end()).unwrap();
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    bench.sim.fail_write(1);
    fail_events_write(&bench, nth);
    let ended = close(session, run);
    assert_eq!(ended.stop, Some(Stop::Signal));
    assert!(
        matches!(ended.errors.as_slice(), [SessionError::Journal(_)]),
        "{:?}",
        ended.errors
    );
    let kinds = kinds(ended.journal.events());
    assert!(kinds.contains(&"smu.error"));
    assert!(!kinds.contains(&"deadend"));
    assert!(!kinds.contains(&"profile.restored"));
    assert!(!kinds.contains(&"shutdown"));
    let Some(emergency) = ended.emergency else {
        panic!("no emergency");
    };
    assert!(matches!(&emergency.response, Response::Zeroed(zeroed) if zeroed.is_clean()));
    assert_eq!(smu_offsets(&bench), [0, 0]);
}

#[test]
fn a_dead_end_a_failed_restoration_cannot_record_withholds_the_emergency_without_a_validated_smu() {
    let nth = u64::try_from(appends_before_the_restoration_dead_end()).unwrap();
    let bench = Bench::new(2);
    let mut session = tuned(&bench);
    let run = Ok(signalled(&mut session));
    bench.sim.fail_write(1);
    fail_events_write(&bench, nth);
    session.validated = None;
    let ended = close(session, run);
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::Journal(_)]
    ));
    assert!(!kinds(ended.journal.events()).contains(&"shutdown"));
    assert!(matches!(
        ended.emergency.unwrap().response,
        Response::Withheld(Withheld::NoValidation)
    ));
    assert_eq!(smu_offsets(&bench), [-5, -5]);
}

/// Applies `offsets` as the Alone condition's profile, the way a trial's setup does.
fn put_alone(session: &mut Session, values: &[i64]) {
    let target = crate::testing::offsets(values);
    let mut check = crate::write::checker(session.appender.tuner());
    crate::write::apply_profile(
        &mut session.appender,
        &mut *session.smu,
        session.lifecycle.stopped().unwrap(),
        &mut session.hardware,
        crate::write::ProfileRequest {
            target: &target,
            causes: &[],
            record: crate::write::ProfileRecord::Applied(togi_machine::Condition::Alone),
        },
        &mut check,
    )
    .unwrap();
}

#[test]
fn a_crash_at_the_restoration_intent_is_classified_by_the_condition_applied_before_it() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    recorded_baseline(&mut session, &[0, 0]);
    put_alone(&mut session, &[-5, -5]);
    session.validated = Some(session.host.validate_smu().unwrap());
    let run = Ok(signalled(&mut session));
    let baseline = session.appender().recovery().baseline().unwrap().0;
    bench.sim.crash_at_write(1);
    let ended = close(session, run);
    assert!(ended.crashed(), "{:?}", ended.errors);
    let events = ended.journal.events();
    let restoring = events.last().unwrap();
    assert!(matches!(restoring.payload, Payload::SmuIntent(_)));
    assert_eq!(restoring.cause, [baseline]);
    assert!(!kinds(events).contains(&"profile.restored"));
    drop(ended);

    bench.sim.reboot().unwrap();
    let input = bench.input();
    input.cancel.cancel();
    let resumed = crate::run(input);
    let detected: Vec<_> = resumed
        .journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::CrashDetected(detected) = &event.payload {
                Some(detected.condition)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(detected, [Some(togi_machine::Condition::Alone)]);
}

#[derive(Clone, Copy, Debug)]
enum Edge {
    TogetherTrialEnd,
    TogetherFailure,
    UnattributedTrialEnd,
    UnattributedFailure,
    ZeroTrialEnd,
    ZeroFailure,
    HuntEnd,
    Combination,
}

fn edge_bench(edge: Edge) -> Bench {
    let zero = matches!(edge, Edge::ZeroTrialEnd | Edge::ZeroFailure);
    let joint = !matches!(edge, Edge::TogetherTrialEnd | Edge::TogetherFailure);
    let mut bench = Bench::defined(&MachineDefinition {
        cores: 4,
        bios: Some(if zero {
            vec![0; 4]
        } else {
            vec![-40, -40, 0, 0]
        }),
        ranking: Some(vec![4, 3, 2, 1]),
        core: (0..4)
            .map(|id| CoreDef {
                id,
                alone: [-31; 5],
                together: [if id == 0 && !joint { -20 } else { -31 }; 7],
                flat: 0.0,
                idle: None,
                workload: BTreeMap::new(),
            })
            .collect(),
        joint: if joint {
            let at = if zero { 0 } else { -30 };
            vec![JointDef {
                members: [(0, at), (1, at)].into(),
                regimes: vec![Regime::R6],
                rate: Some(1e6),
                signal: Some(Signal::CorrectedMce),
                ..JointDef::default()
            }]
        } else {
            Vec::new()
        },
        model: ModelDef {
            core_local_bank: if joint {
                0.0
            } else {
                ModelDef::default().core_local_bank
            },
            signals: Weights::from([(Signal::ComputationError, 1.0)]),
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    });
    let limit = if zero {
        0
    } else if joint {
        -31
    } else {
        -30
    };
    let mut config = format!(
        r#"{{"candidate_solo_limits": {{"0": {limit}, "1": {limit}, "2": {limit}, "3": {limit}}},"#
    );
    if joint {
        config.push_str(r#""checking": {"cycle": ["R6"]},"#);
    }
    config.push_str(
        r#""durations": {"search_trial_s": 1, "short_trial_s": 1, "checking_trial_s": 1, "checking_all_core_s": 4},
        "evidence": {"rate": 0.95, "miss": 0.2}}"#,
    );
    bench.config = togi_config::Config::parse(&config).unwrap();
    bench
}

/// The most lines of the run log a drive reads before it cancels the run, so a boundary that is never reached fails
/// the test instead of running on.
const LOG_LINES: usize = 3000;

/// Cancels the run at the first log line `edge` names, or after [`LOG_LINES`] lines.
struct CancelAt {
    edge: Edge,
    cancel: togi_machine::CancellationToken,
    hit: std::rc::Rc<std::cell::Cell<bool>>,
    lines: usize,
    line: String,
}

impl Edge {
    fn names(self, kind: &str, line: &str) -> bool {
        let unattributed = line.contains(" unattributed ");
        match self {
            Self::TogetherTrialEnd | Self::UnattributedTrialEnd | Self::ZeroTrialEnd => {
                kind == "trial.end" && line.contains(" FAIL ")
            }
            Self::TogetherFailure => kind == "failure" && !unattributed,
            Self::UnattributedFailure | Self::ZeroFailure => kind == "failure" && unattributed,
            Self::HuntEnd => kind == "hunt.end" && line.contains("combination"),
            Self::Combination => kind == "combination",
        }
    }

    const fn zero(self) -> bool {
        matches!(self, Self::ZeroTrialEnd | Self::ZeroFailure)
    }

    /// Whether the stop owes a back-off: a failure no single core owns, and one at zero, owes none.
    const fn owes_a_move(self) -> bool {
        !matches!(
            self,
            Self::UnattributedTrialEnd
                | Self::UnattributedFailure
                | Self::ZeroTrialEnd
                | Self::ZeroFailure
        )
    }
}

impl std::io::Write for CancelAt {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.line.push_str(&String::from_utf8_lossy(bytes));
        while let Some(end) = self.line.find('\n') {
            let line: String = self.line.drain(..=end).collect();
            self.lines += 1;
            let kind = line.split_whitespace().nth(1).unwrap_or_default();
            if self.edge.names(kind, &line) {
                self.hit.set(true);
                self.cancel.cancel();
            }
            if self.lines >= LOG_LINES {
                self.cancel.cancel();
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The first journal event `edge` names.
fn edge_event(events: &[Event], edge: Edge) -> Option<usize> {
    events.iter().position(|event| {
        let payload = &event.payload;
        if let Payload::TrialEnd(end) = payload {
            matches!(
                edge,
                Edge::TogetherTrialEnd | Edge::UnattributedTrialEnd | Edge::ZeroTrialEnd
            ) && end.outcome == togi_journal::Outcome::Failure
        } else if let Payload::Failure(failure) = payload {
            let unattributed = failure.attribution == togi_journal::Attribution::Unattributed;
            match edge {
                Edge::TogetherFailure => !unattributed,
                Edge::UnattributedFailure | Edge::ZeroFailure => unattributed,
                Edge::TogetherTrialEnd
                | Edge::UnattributedTrialEnd
                | Edge::ZeroTrialEnd
                | Edge::HuntEnd
                | Edge::Combination => false,
            }
        } else if let Payload::HuntEnd(end) = payload {
            edge_is(edge, Edge::HuntEnd) && end.result == togi_journal::HuntOutcome::Combination
        } else {
            matches!(payload, Payload::Combination(_)) && edge_is(edge, Edge::Combination)
        }
    })
}

fn edge_is(edge: Edge, other: Edge) -> bool {
    std::mem::discriminant(&edge) == std::mem::discriminant(&other)
}

/// Stops a whole run at `edge` and returns what the stop did after it.
fn stopped_at(edge: Edge) -> (Bench, Ended) {
    let bench = edge_bench(edge);
    let hit = std::rc::Rc::new(std::cell::Cell::new(false));
    for _ in 0..40 {
        let mut input = bench.input();
        input.bootloader = Some(Box::new(crate::testing::MemBoot::default()));
        input.log = Some(Box::new(CancelAt {
            edge,
            cancel: input.cancel.clone(),
            hit: hit.clone(),
            lines: 0,
            line: String::new(),
        }));
        let ended = crate::run(input);
        if ended.crashed() {
            bench.sim.reboot().unwrap();
            continue;
        }
        assert!(hit.get(), "{edge:?} was never reached: {:?}", ended.errors);
        return (bench, ended);
    }
    panic!("{edge:?}: the machine kept crashing");
}

#[test_case(Edge::TogetherTrialEnd; "Edge::TogetherTrialEnd")]
#[test_case(Edge::TogetherFailure; "Edge::TogetherFailure")]
#[test_case(Edge::UnattributedTrialEnd; "Edge::UnattributedTrialEnd")]
#[test_case(Edge::UnattributedFailure; "Edge::UnattributedFailure")]
#[test_case(Edge::ZeroTrialEnd; "Edge::ZeroTrialEnd")]
#[test_case(Edge::ZeroFailure; "Edge::ZeroFailure")]
#[test_case(Edge::HuntEnd; "Edge::HuntEnd")]
#[test_case(Edge::Combination; "Edge::Combination")]
fn a_stop_at_a_durable_boundary_drains_what_it_owes_and_starts_no_new_work(edge: Edge) {
    let (bench, ended) = stopped_at(edge);
    assert!(ended.errors.is_empty(), "{edge:?}: {:?}", ended.errors);
    assert!(ended.emergency.is_none(), "{edge:?}");
    let events = ended.journal.events();
    let boundary = edge_event(events, edge).unwrap();
    let mut restored = None;
    let mut moves = 0;
    for event in &events[boundary + 1..] {
        match &event.payload {
            Payload::TrialIntent(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::DeepeningRound(_)
            | Payload::HostRanking(_) => {
                panic!("{edge:?}: new work during the drain: {event:?}")
            }
            Payload::TunerDecision(decision) => {
                assert!(
                    matches!(
                        decision.decision,
                        togi_journal::Decision::Backoff | togi_journal::Decision::CheckSoloLimit
                    ),
                    "{edge:?}: a new move during the drain: {decision:?}"
                );
                moves += 1;
            }
            Payload::ProfileRestored(profile) => restored = Some(profile.offsets),
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayUncleanEnd(_)
            | Payload::StateRebuilt(_) => {}
        }
    }
    assert_eq!(moves, usize::from(edge.owes_a_move()), "{edge:?}");
    let restored = restored.unwrap_or_else(|| {
        if edge.zero() {
            crate::testing::offsets(&[0; 4])
        } else {
            let mut smu = bench.sim.seams().smu;
            let held: Vec<i64> = (0..4).map(|n| smu.offset(core(n)).unwrap().0).collect();
            if matches!(edge, Edge::HuntEnd | Edge::Combination) {
                assert_eq!(held, [-30, -29, 0, 0], "{edge:?}");
            }
            crate::testing::offsets(&held)
        }
    });
    for event in events {
        if let Payload::Failure(failure) = &event.payload {
            if let (Some(failed), Some(offset)) = (failure.core, failure.offset) {
                assert!(
                    restored.get(failed).unwrap() > offset,
                    "{edge:?}: {restored:?} reaches {failure:?}"
                );
            }
        } else if let Payload::Combination(combination) = &event.payload {
            let reaches = combination
                .members
                .iter()
                .all(|member| restored.get(member.core).unwrap() <= member.offset);
            assert!(!reaches, "{edge:?}: {restored:?} reaches {combination:?}");
        }
    }
    let mut smu = bench.sim.seams().smu;
    let actual: Vec<i64> = (0..4).map(|n| smu.offset(core(n)).unwrap().0).collect();
    let want: Vec<i64> = (0..4)
        .map(|n| restored.get(core(n)).unwrap().get())
        .collect();
    assert_eq!(actual, want, "{edge:?}");
    if edge.zero() {
        let Some(Stop::DeadEnd { dead_end, .. }) = &ended.stop else {
            panic!("{edge:?}: {:?}", ended.stop);
        };
        assert_eq!(dead_end.condition, DeadEndCondition::FailureAtZero);
        let cleared = events
            .iter()
            .filter(|event| matches!(event.payload, Payload::BootSavedEntry(_)))
            .count();
        assert_eq!(cleared, 1, "{edge:?}");
    } else {
        assert_eq!(ended.stop, Some(Stop::Signal), "{edge:?}");
    }
}

/// A panic that is the only failure strikes the restoration's own append: cleanup must not unwind
/// out of the close with the tuned offsets still on the machine.
#[test]
fn a_panic_taking_in_the_restoration_intent_of_an_ordinary_stop_gets_the_emergency_and_is_handed_back()
 {
    let bench = Bench::new(2);
    let armed = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = armed.clone();
    let mut input = bench.input();
    input.on_append = Some(Box::new(move |event| {
        assert!(
            !(flag.get() && event.kind() == togi_journal::EventKind::SmuIntent),
            "a bug taking in the restoration intent"
        );
    }));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    assert_eq!(session.start().unwrap(), crate::session::Control::Continue);
    assert_eq!(session.sweep().unwrap(), crate::session::Control::Continue);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, &[-5, -5]);
    session.validated = Some(session.host.validate_smu().unwrap());
    let run = Ok(signalled(&mut session));
    armed.set(true);
    let mut ended = session.close(run);
    let panic = ended.panic.take().expect("the panic is handed back");
    assert_eq!(panic.message(), "a bug taking in the restoration intent");
    assert!(matches!(
        ended.errors.as_slice(),
        [SessionError::Panicked(message)] if message == "a bug taking in the restoration intent"
    ));
    let kinds = kinds(ended.journal.events());
    assert!(!kinds.contains(&"profile.restored"), "{kinds:?}");
    assert!(!kinds.contains(&"shutdown"), "no clean stop is claimed");
    assert!(
        matches!(
            ended.emergency.as_ref().map(|outcome| &outcome.response),
            Some(Response::Zeroed(zeroed)) if zeroed.is_clean()
        ),
        "{:?}",
        ended.emergency
    );
    assert_eq!(smu_offsets(&bench), [0, 0]);
}
