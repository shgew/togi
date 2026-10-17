use std::cell::{Cell, RefCell};
use std::io;
use std::num::NonZeroU32;
use std::rc::Rc;

use togi_config::Config;
use togi_defect::{Clause, DefectId, Entry};
use togi_journal::fs::{Fault as FsFault, Op};
use togi_journal::{
    Activity, Decision, DefectDirection, Event, EventKind, Payload, RetryTarget, SessionContext,
    ShutdownReason, TunerDecision,
};
use togi_machine::{
    BootId, CancellationToken, Clock, Error, Host, Kernel, KernelCursor, KernelRead, Partial,
    PstoreRecord, Reporter, ResetKind, ResetReason, Running, Signal, Started, Stopped, Swept,
    TrialResult, TrialSpec, Trials,
};
use togi_sim::MachineDefinition;
use togi_sim::definition::ModelDef;
use togi_sim::faults::{HookPhase, SmuOperation};
use togi_sim::model::Script;
use togi_tuningboot::{Bootloader, Environment, SavedEntry};

use super::*;
use crate::emergency::Response;
use crate::session::{ConfigSource, Fault, Lifecycle, Prompt, PromptError, RunInput, Stop};
use crate::testing::{
    Bench, MemBoot, SweepFails, ValidationFails, core, kinds_of, offsets, put, recorded_baseline,
    swept,
};
use crate::{Ended, run};

fn started(bench: &Bench) -> Session<'static> {
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
        .filter_map(|event| {
            if let Payload::PreflightCheck(check) = &event.payload {
                Some((check.check.clone(), check.ok))
            } else {
                None
            }
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
    assert!(matches!(session.lifecycle, Lifecycle::Unswept));
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
    assert!(session.lifecycle.stopped().is_some());
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
    assert_eq!(session.lifecycle.fault(), Some(Fault::Containment));
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
fn a_failed_sweep_journals_the_operation_and_its_source() {
    let bench = Bench::new(2);
    let mut input = bench.input();
    let trials = input.machine.trials;
    input.machine.trials = Box::new(SweepFails(trials, || {
        Error::Containment(Box::new(std::io::Error::other("scope busy")))
    }));
    let Ok(mut session) = Session::open(input) else {
        panic!("open");
    };
    session.start().unwrap();
    session.preflight().unwrap();
    let (dead_end, _) = dead_end_of(session.sweep().unwrap());
    let detail = "trial cleanup could not be confirmed: scope busy";
    assert_eq!(dead_end.detail, detail);
    let event = session.appender().journal().events().last().unwrap();
    assert_eq!(event.msg, format!("dead end containment: {detail}"));
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
    assert_eq!(session.lifecycle.fault(), Some(Fault::Containment));
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

fn started_with_failing_validation(bench: &Bench, error: fn() -> Error) -> Session<'static> {
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
        .find_map(|event| {
            if let Payload::SessionWarning(warning) = &event.payload {
                Some(warning.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(warning.operation, "validate the SMU interface");
    assert!(warning.error.contains("no SMU driver"));
}

fn from_the_tuning_boot(bench: &Bench, cancel: CancellationToken) -> Session<'static> {
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

fn left_applied(bench: &Bench, values: &[i64]) -> Session<'static> {
    let mut session = swept(bench);
    recorded_baseline(&mut session, &[0, 0]);
    put(&mut session, values);
    session
}

fn resumed(bench: &Bench) -> Session<'static> {
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

#[derive(Clone, Debug, Eq, PartialEq)]
enum Touch {
    Sweep,
    Ended(String),
    Start,
}

type Touched = Rc<RefCell<Vec<Touch>>>;

/// What a trial start does after the simulator started it.
enum AtStart {
    /// Asks the run to stop, which ends it after this trial.
    Cancel(CancellationToken),
    /// The process dies while the trial runs, leaving it open in this boot.
    Die,
}

/// The simulator's trials, with every sweep, end and start written down.
struct Observed {
    inner: Box<dyn Trials>,
    touched: Touched,
    sweep_fails: bool,
    at_start: AtStart,
}

impl Trials for Observed {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, Error> {
        self.touched.borrow_mut().push(Touch::Sweep);
        if self.sweep_fails {
            return Err(Error::Containment("scope busy".into()));
        }
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        self.touched.borrow_mut().push(Touch::Start);
        let running = self.inner.start(cancel, spec, ready)?;
        match &self.at_start {
            AtStart::Cancel(signal) => {
                signal.cancel();
                Ok(running)
            }
            AtStart::Die => Ok(Box::new(DiesInWait(running))),
        }
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.touched.borrow_mut().push(Touch::Ended(id.to_owned()));
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
        self.inner.samples(id)
    }
}

struct DiesInWait(Box<dyn Running>);

impl Running for DiesInWait {
    fn started(&self) -> Started {
        self.0.started()
    }

    fn wait(
        &mut self,
        _cancel: &CancellationToken,
        _report: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        Err(Partial {
            value: TrialResult::default(),
            error: Error::Crashed,
        })
    }

    fn stop(self: Box<Self>) -> Result<Stopped, Error> {
        self.0.stop()
    }
}

/// Runs `input` with observed trials that stop the run once its first trial started.
fn observed_run(mut input: RunInput<'static>, sweep_fails: bool) -> (Ended, Vec<Touch>) {
    let touched = Touched::default();
    let at_start = AtStart::Cancel(input.cancel.clone());
    input.machine.trials = Box::new(Observed {
        inner: input.machine.trials,
        touched: Rc::clone(&touched),
        sweep_fails,
        at_start,
    });
    let ended = run(input);
    let touched = touched.take();
    (ended, touched)
}

/// Every SMU access from now on, as the simulator is asked for it.
fn smu_accesses(bench: &Bench) -> Rc<RefCell<Vec<SmuOperation>>> {
    let accesses = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&accesses);
    bench.sim.interrupt_smu(move |operation: &SmuOperation| {
        if operation.phase == HookPhase::Before {
            log.borrow_mut().push(*operation);
        }
        Ok(())
    });
    accesses
}

fn offsets_on(bench: &Bench) -> Vec<i64> {
    bench.sim.interrupt_smu(|_: &SmuOperation| Ok(()));
    let mut smu = bench.sim.seams().smu;
    (0..2).map(|n| smu.offset(core(n)).unwrap().0).collect()
}

fn position_of(events: &[Event], kind: EventKind) -> Option<usize> {
    events.iter().position(|event| event.kind() == kind)
}

fn any_of(events: &[Event], kinds: &[EventKind]) -> bool {
    events.iter().any(|event| kinds.contains(&event.kind()))
}

fn stopped_dead_end(ended: &Ended) -> (&togi_journal::DeadEnd, &[Seq]) {
    let Some(Stop::DeadEnd {
        dead_end, causes, ..
    }) = &ended.stop
    else {
        panic!("{:?}, {:?}", ended.stop, ended.errors);
    };
    (dead_end, causes)
}

fn swept_ok(events: &[Event]) -> Option<Seq> {
    events.iter().find_map(|event| {
        if let Payload::PreflightCheck(check) = &event.payload
            && check.check == "trial_scopes"
            && check.ok
        {
            Some(event.seq)
        } else {
            None
        }
    })
}

fn trial_end(events: &[Event], trial: &str) -> Option<Seq> {
    events.iter().find_map(|event| {
        if let Payload::TrialEnd(end) = &event.payload
            && end.trial.as_str() == trial
        {
            Some(event.seq)
        } else {
            None
        }
    })
}

#[test]
fn a_run_whose_preflight_fails_never_sweeps_writes_or_starts_a_trial() {
    let bench = Bench::new(2);
    bench.sim.fail_check("root", "not root");
    let accesses = smu_accesses(&bench);
    let (ended, touched) = observed_run(bench.input(), false);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let (dead_end, causes) = stopped_dead_end(&ended);
    assert_eq!(dead_end.condition, DeadEndCondition::Preflight);
    assert_eq!(dead_end.detail, "failed checks: root (not root)");
    assert_eq!(causes.len(), 1);
    assert_eq!(touched, []);
    assert!(
        accesses
            .borrow()
            .iter()
            .all(|access| access.op == togi_sim::faults::SmuOp::Read),
        "{:?}",
        accesses.borrow()
    );
    let events = ended.journal.events();
    assert!(!any_of(
        events,
        &[
            EventKind::SmuIntent,
            EventKind::SmuWrite,
            EventKind::TrialIntent
        ]
    ));
    assert!(!events.iter().any(
        |event| matches!(&event.payload, Payload::PreflightCheck(check) if check.check == "trial_scopes")
    ));
}

/// A clock whose sleeps also move the simulator's, and that counts how long the run slept.
struct PolledClock {
    inner: Box<dyn Clock>,
    slept: Rc<Cell<Duration>>,
    signal: Option<CancellationToken>,
    fails: bool,
}

impl Clock for PolledClock {
    fn now(&self) -> jiff::Timestamp {
        self.inner.now()
    }

    fn monotonic(&self) -> Duration {
        self.inner.monotonic()
    }

    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        if self.fails {
            return Err(Error::Other("clock unavailable".into()));
        }
        self.inner.sleep(&CancellationToken::new(), duration)?;
        self.slept.set(self.slept.get() + duration);
        if let Some(signal) = &self.signal {
            signal.cancel();
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
}

/// A host whose hardware watchdog arms once the run slept `ready_at`, and that notes when it was asked.
struct ArmsLate {
    inner: Box<dyn Host>,
    slept: Rc<Cell<Duration>>,
    ready_at: Duration,
    polls: Rc<RefCell<Vec<Duration>>>,
}

impl Host for ArmsLate {
    fn boot_id(&mut self) -> Result<BootId, Error> {
        self.inner.boot_id()
    }

    fn topology(&mut self) -> Result<Vec<togi_machine::CoreInfo>, Error> {
        self.inner.topology()
    }

    fn validate_smu(&mut self) -> Result<togi_machine::SmuValidated, Error> {
        self.inner.validate_smu()
    }

    fn bios_context(&mut self) -> Result<togi_machine::BiosContext, Error> {
        self.inner.bios_context()
    }

    fn ranking(
        &mut self,
    ) -> Result<Vec<togi_machine::CoreRank>, Partial<Vec<togi_machine::CoreRank>>> {
        self.inner.ranking()
    }

    fn preflight(&mut self) -> Vec<togi_machine::Check> {
        self.inner.preflight()
    }

    fn watchdog(&mut self) -> togi_machine::Check {
        let at = self.slept.get();
        self.polls.borrow_mut().push(at);
        togi_machine::Check {
            name: "watchdog".into(),
            detail: "test hardware watchdog".into(),
            ok: at >= self.ready_at,
        }
    }
}

/// A bootloader that counts how often its saved entry is cleared.
struct CountedBoot {
    env: MemBoot,
    clears: Rc<Cell<u32>>,
}

impl Environment for CountedBoot {
    fn get(&mut self, name: &str) -> io::Result<Option<String>> {
        self.env.get(name)
    }

    fn set(&mut self, values: &[(&str, &str)]) -> io::Result<()> {
        self.env.set(values)
    }

    fn unset(&mut self, names: &[&str]) -> io::Result<()> {
        self.env.unset(names)
    }
}

impl Bootloader for CountedBoot {
    fn clear_saved_entry(&mut self) -> io::Result<SavedEntry> {
        self.clears.set(self.clears.get() + 1);
        self.env.clear_saved_entry()
    }
}

#[derive(Debug)]
enum Waited {
    Trial,
    DeadEnd,
    Signal,
    ClockError,
}

#[test]
fn the_watchdog_wait_polls_each_second_until_armed_or_thirty_seconds_and_stops_on_a_signal_or_clock_error()
 {
    let cases = [
        ("already armed", 0, false, false, 0, Waited::Trial),
        ("late arm", 3, false, false, 3, Waited::Trial),
        ("deadline arm", 30, false, false, 30, Waited::Trial),
        ("too late", 31, false, false, 30, Waited::DeadEnd),
        (
            "signal while sleeping",
            3600,
            true,
            false,
            1,
            Waited::Signal,
        ),
        ("clock error", 3600, false, true, 0, Waited::ClockError),
    ];
    for (case, ready_at, signal, fails, waited, outcome) in cases {
        let bench = Bench::new(2);
        let slept = Rc::new(Cell::new(Duration::ZERO));
        let polls = Rc::new(RefCell::new(Vec::new()));
        let clears = Rc::new(Cell::new(0));
        let mut input = bench.input();
        input.bootloader = Some(Box::new(CountedBoot {
            env: MemBoot::default(),
            clears: Rc::clone(&clears),
        }));
        input.machine.clock = Box::new(PolledClock {
            inner: input.machine.clock,
            slept: Rc::clone(&slept),
            signal: signal.then(|| input.cancel.clone()),
            fails,
        });
        input.machine.host = Box::new(ArmsLate {
            inner: input.machine.host,
            slept: Rc::clone(&slept),
            ready_at: Duration::from_secs(ready_at),
            polls: Rc::clone(&polls),
        });
        let accesses = smu_accesses(&bench);
        let (ended, touched) = observed_run(input, false);
        let expected_polls: Vec<Duration> = match outcome {
            Waited::Signal | Waited::ClockError => vec![Duration::ZERO],
            Waited::Trial | Waited::DeadEnd => (0..=waited).map(Duration::from_secs).collect(),
        };
        assert_eq!(*polls.borrow(), expected_polls, "{case}");
        let events = ended.journal.events();
        let watchdog: Vec<(bool, &str)> = events
            .iter()
            .filter_map(|event| {
                if let Payload::PreflightCheck(check) = &event.payload
                    && check.check == "watchdog"
                {
                    Some((check.ok, check.detail.as_str()))
                } else {
                    None
                }
            })
            .collect();
        let detail =
            format!("test hardware watchdog; waited {waited}s for an armed hardware watchdog");
        match outcome {
            Waited::Trial => {
                assert!(ended.errors.is_empty(), "{case}: {:?}", ended.errors);
                assert_eq!(ended.stop, Some(Stop::Signal), "{case}");
                assert_eq!(watchdog, [(true, detail.as_str())], "{case}");
                assert!(touched.contains(&Touch::Start), "{case}: {touched:?}");
                let armed = events
                    .iter()
                    .position(|event| matches!(&event.payload, Payload::PreflightCheck(check) if check.check == "watchdog"))
                    .unwrap();
                let first_action = events
                    .iter()
                    .position(|event| {
                        matches!(event.kind(), EventKind::SmuIntent | EventKind::TrialIntent)
                    })
                    .unwrap();
                assert!(armed < first_action, "{case}");
            }
            Waited::DeadEnd => {
                assert!(ended.errors.is_empty(), "{case}: {:?}", ended.errors);
                let (dead_end, causes) = stopped_dead_end(&ended);
                assert_eq!(dead_end.condition, DeadEndCondition::Preflight, "{case}");
                assert_eq!(
                    dead_end.detail,
                    format!("failed checks: watchdog ({detail})"),
                    "{case}"
                );
                assert_eq!(causes.len(), 1, "{case}");
                assert_eq!(watchdog, [(false, detail.as_str())], "{case}");
                assert_eq!(clears.get(), 1, "{case}");
            }
            Waited::Signal => {
                assert!(ended.errors.is_empty(), "{case}: {:?}", ended.errors);
                assert_eq!(ended.stop, Some(Stop::Signal), "{case}");
                assert_eq!(watchdog, [], "{case}");
            }
            Waited::ClockError => {
                assert!(
                    matches!(
                        ended.errors.first(),
                        Some(SessionError::Machine(Error::Other(detail))) if detail.to_string() == "clock unavailable"
                    ),
                    "{case}: {:?}",
                    ended.errors
                );
                assert_eq!(watchdog, [], "{case}");
            }
        }
        assert!(slept.get() >= Duration::from_secs(waited), "{case}");
        if !matches!(outcome, Waited::Trial) {
            assert_eq!(slept.get(), Duration::from_secs(waited), "{case}");
            assert_eq!(touched, [], "{case}");
            assert!(
                accesses.borrow().is_empty(),
                "{case}: {:?}",
                accesses.borrow()
            );
            assert!(
                !any_of(
                    events,
                    &[
                        EventKind::SmuIntent,
                        EventKind::SmuWrite,
                        EventKind::TrialIntent
                    ]
                ),
                "{case}"
            );
        }
    }
}

fn crashing_bench() -> Bench {
    let mut bench = Bench::defined(&MachineDefinition {
        cores: 2,
        model: ModelDef {
            past_limit_rate: 0.0,
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    });
    bench.config = Config::parse(r#"{"start_offsets":{"0":-10,"1":-10}}"#).unwrap();
    bench
}

/// A run whose first trial took the machine down; returns the crashed boot, after the reboot.
fn crashed_in_its_first_trial(bench: &Bench) -> BootId {
    bench
        .sim
        .script_trial(
            "0001",
            Script {
                signal: Some(Signal::Crash),
                at_s: 2.0,
                core: core(0),
                reset: Some(ResetKind::Watchdog),
                then_crash: false,
            },
        )
        .unwrap();
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    let crashed = run(input);
    assert!(crashed.crashed(), "{:?}", crashed.errors);
    let boot = crashed.journal.events().last().unwrap().boot.clone();
    drop(crashed);
    bench.sim.reboot();
    boot
}

/// The kernel log of `boot` fails its first `limit` reads, or every read without one.
struct Unreadable {
    inner: Box<dyn Kernel>,
    boot: BootId,
    failed: u32,
    limit: Option<u32>,
}

impl Unreadable {
    fn fails(&mut self, boot: &BootId) -> Option<Error> {
        if *boot != self.boot || self.limit.is_some_and(|limit| self.failed >= limit) {
            return None;
        }
        self.failed += 1;
        Some(Error::Other("simulated unreadable kernel log".into()))
    }
}

impl Kernel for Unreadable {
    fn mces(
        &mut self,
        boot: &BootId,
        since: Duration,
    ) -> Result<Vec<togi_machine::Mce>, Partial<Vec<togi_machine::Mce>>> {
        match self.fails(boot) {
            Some(error) => Err(Partial {
                value: Vec::new(),
                error,
            }),
            None => self.inner.mces(boot, since),
        }
    }

    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<togi_machine::Mce>>> {
        match self.fails(boot) {
            Some(error) => Err(Partial {
                value: Vec::new(),
                error,
            }),
            None => self.inner.read_mces(boot, after),
        }
    }

    fn reset_reason(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        match self.fails(boot) {
            Some(error) => Err(error),
            None => self.inner.reset_reason(boot),
        }
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, Error> {
        match self.fails(boot) {
            Some(error) => Err(error),
            None => self.inner.reset_reason_after(boot),
        }
    }

    fn saved_pstore(&mut self, boot: &BootId) -> Result<Option<PstoreRecord>, Error> {
        self.inner.saved_pstore(boot)
    }
}

/// The machine's clock, noting every wait.
struct Waits {
    inner: Box<dyn Clock>,
    waits: Rc<RefCell<Vec<Duration>>>,
}

impl Clock for Waits {
    fn now(&self) -> jiff::Timestamp {
        self.inner.now()
    }

    fn monotonic(&self) -> Duration {
        self.inner.monotonic()
    }

    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        self.waits.borrow_mut().push(duration);
        self.inner.sleep(cancel, duration)
    }
}

fn kernel_retries(events: &[Event]) -> Vec<Seq> {
    events
        .iter()
        .filter(|event| matches!(&event.payload, Payload::BackendRetry(retry) if retry.backend == RetryTarget::KernelLog))
        .map(|event| event.seq)
        .collect()
}

#[test]
fn a_run_cancelled_while_recovery_waits_reconciles_the_inherited_offsets_before_its_signal_shutdown()
 {
    let bench = crashing_bench();
    let crashed = crashed_in_its_first_trial(&bench);
    let left = left_applied(&bench, &[-30, -20]);
    let before = left.appender().journal().events().len();
    drop(left);

    let waits = Rc::new(RefCell::new(Vec::new()));
    let mut input = bench.input();
    input.cancel.cancel();
    input.bootloader = Some(Box::new(MemBoot::default()));
    input.machine.kernel = Box::new(Unreadable {
        inner: input.machine.kernel,
        boot: crashed,
        failed: 0,
        limit: Some(1),
    });
    input.machine.clock = Box::new(Waits {
        inner: input.machine.clock,
        waits: Rc::clone(&waits),
    });
    let (ended, touched) = observed_run(input, false);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(Stop::Signal));
    assert_eq!(*waits.borrow(), [Duration::from_secs(60)]);
    assert_eq!(touched, [Touch::Sweep]);
    let events = &ended.journal.events()[before..];
    assert_eq!(kernel_retries(events).len(), 1);
    let restored = position_of(events, EventKind::ProfileRestored).unwrap();
    let shutdown = position_of(events, EventKind::Shutdown).unwrap();
    assert!(
        restored < shutdown,
        "restored {restored}, shutdown {shutdown}"
    );
    assert!(matches!(&events[shutdown].payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal));
    let Payload::ProfileRestored(profile) = &events[restored].payload else {
        unreachable!()
    };
    let restored: Vec<i64> = profile
        .offsets
        .iter()
        .map(|(_, offset)| offset.get())
        .collect();
    assert_eq!(offsets_on(&bench), restored);
    assert_ne!(restored, [-30, -20]);
    assert!(!any_of(
        events,
        &[EventKind::TrialIntent, EventKind::CrashDetected]
    ));
}

#[test]
fn a_kernel_log_unreadable_through_every_recovery_retry_is_a_no_evidence_dead_end_citing_them() {
    let bench = crashing_bench();
    let crashed = crashed_in_its_first_trial(&bench);
    let mut input = bench.input();
    input.policy.cycles = Some(NonZeroU32::MIN);
    input.machine.kernel = Box::new(Unreadable {
        inner: input.machine.kernel,
        boot: crashed.clone(),
        failed: 0,
        limit: None,
    });
    let (ended, touched) = observed_run(input, false);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let current = bench.sim.seams().host.boot_id().unwrap();
    let events: Vec<Event> = ended
        .journal
        .events()
        .iter()
        .filter(|event| event.boot == current)
        .cloned()
        .collect();
    let retries = kernel_retries(&events);
    assert_eq!(retries.len(), 3);
    let (dead_end, causes) = stopped_dead_end(&ended);
    assert_eq!(dead_end.condition, DeadEndCondition::NoEvidence);
    assert!(
        dead_end.detail.starts_with(&format!(
            "kernel log of boot {crashed} unreadable after retries at 1, 5 and 30 min"
        )),
        "{}",
        dead_end.detail
    );
    assert_eq!(causes, retries.as_slice());
    assert!(!touched.contains(&Touch::Start), "{touched:?}");
    assert!(!any_of(
        &events,
        &[EventKind::TrialIntent, EventKind::CrashDetected]
    ));
}

const DEEPER_ON_WARNING: Clause = Clause {
    decision_kind: EventKind::TunerDecision,
    decision: Decision::StepDeeper,
    cause_kind: EventKind::SessionWarning,
};

static AGGRESSIVE: [Entry; 1] = [Entry {
    id: DefectId::new(1).unwrap(),
    title: "synthetic",
    detail: "synthetic detail",
    pr: 100,
    direction: DefectDirection::TooAggressive,
    clauses: &[DEEPER_ON_WARNING],
}];

/// A journal whose earlier run of this boot made a decision the too-aggressive defect matches.
fn with_an_aggressive_decision() -> Bench {
    let mut bench = Bench::new(2);
    bench.defects = &AGGRESSIVE;
    let mut session = bench.open();
    assert_eq!(session.start().unwrap(), Control::Continue);
    let warning = session
        .appender
        .append(
            Payload::SessionWarning(togi_journal::SessionWarning {
                operation: "note".into(),
                trial: None,
                error: String::new(),
            }),
            &[],
        )
        .unwrap();
    let decision = TunerDecision {
        core: core(1),
        activity: Activity::Search,
        decision: Decision::StepDeeper,
        from_offset: togi_machine::TargetOffset::ZERO,
        to_offset: togi_machine::TargetOffset::ZERO,
        pass: None,
        failure_point: None,
        workloads: Vec::new(),
        reason: "synthetic".into(),
    };
    session
        .appender
        .append(Payload::TunerDecision(decision), &[warning])
        .unwrap();
    drop(session);
    bench
}

struct Cancelled;

impl Prompt for Cancelled {
    fn reset_cores(&mut self, _: &togi_defect::FindingView<'_>) -> Result<bool, PromptError> {
        Err(PromptError::Cancelled)
    }
}

#[test]
fn an_unanswered_aggressive_defect_found_at_startup_is_the_runs_dead_end() {
    let bench = with_an_aggressive_decision();
    let (ended, touched) = observed_run(bench.input(), false);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let (dead_end, causes) = stopped_dead_end(&ended);
    assert_eq!(dead_end.condition, DeadEndCondition::Defect);
    let events = ended.journal.events();
    let found = events
        .iter()
        .find(|event| event.kind() == EventKind::DefectFound)
        .unwrap();
    assert_eq!(causes, [found.seq]);
    assert!(!touched.contains(&Touch::Start), "{touched:?}");
    assert!(!any_of(events, &[EventKind::TrialIntent]));
}

#[test]
fn a_defect_prompt_cancelled_at_startup_stops_the_run_before_any_trial() {
    let bench = with_an_aggressive_decision();
    let mut input = bench.input();
    input.prompt = Some(Box::new(Cancelled));
    let (ended, touched) = observed_run(input, false);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    assert_eq!(ended.stop, Some(Stop::Signal));
    assert!(!touched.contains(&Touch::Start), "{touched:?}");
    let events = ended.journal.events();
    assert!(!any_of(events, &[EventKind::TrialIntent]));
    assert!(matches!(&events.last().unwrap().payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal));
}

#[test]
fn a_same_boot_resume_with_an_unsupported_smu_is_a_preflight_dead_end_that_leaves_the_offsets() {
    for check in ["cpu", "ryzen_smu"] {
        let bench = Bench::new(2);
        let applied = left_applied(&bench, &[-10, -12]);
        let before = applied.appender().journal().events().len();
        drop(applied);
        bench.sim.fail_check(check, "unsupported");
        let accesses = smu_accesses(&bench);
        let (ended, touched) = observed_run(bench.input(), false);
        assert!(ended.errors.is_empty(), "{check}: {:?}", ended.errors);
        let (dead_end, _) = stopped_dead_end(&ended);
        assert_eq!(dead_end.condition, DeadEndCondition::Preflight, "{check}");
        assert!(
            dead_end.detail.contains(&format!("{check} (unsupported)")),
            "{check}: {}",
            dead_end.detail
        );
        assert!(
            accesses.borrow().is_empty(),
            "{check}: {:?}",
            accesses.borrow()
        );
        assert_eq!(touched, [], "{check}");
        let events = &ended.journal.events()[before..];
        assert!(
            !any_of(
                events,
                &[
                    EventKind::SmuIntent,
                    EventKind::SmuWrite,
                    EventKind::SmuReadback,
                    EventKind::ProfileRestored,
                    EventKind::Shutdown,
                    EventKind::TrialIntent,
                ]
            ),
            "{check}: {:?}",
            kinds_of_events(events)
        );
        assert_eq!(offsets_on(&bench), [-10, -12], "{check}");
    }
}

fn kinds_of_events(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(|event| event.kind().as_str()).collect()
}

/// A run of this boot whose process died while its first trial ran; returns that open trial.
fn left_open(bench: &Bench) -> String {
    let touched = Touched::default();
    let mut input = bench.input();
    input.machine.trials = Box::new(Observed {
        inner: input.machine.trials,
        touched: Rc::clone(&touched),
        sweep_fails: false,
        at_start: AtStart::Die,
    });
    let died = run(input);
    assert!(died.crashed(), "{:?}", died.errors);
    assert_eq!(touched.take(), [Touch::Sweep, Touch::Start]);
    let trial = died
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialStart(start) = &event.payload {
                Some(start.trial.as_str().to_owned())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(trial_end(died.journal.events(), &trial), None);
    trial
}

#[derive(Clone, Copy, Debug)]
enum Failing {
    Nothing,
    Preflight,
    Sweep,
}

#[test]
fn a_same_boot_open_trial_is_ended_only_after_a_successful_sweep() {
    for failing in [Failing::Nothing, Failing::Preflight, Failing::Sweep] {
        let bench = Bench::new(2);
        let trial = left_open(&bench);
        if matches!(failing, Failing::Preflight) {
            bench.sim.fail_check("root", "not root");
        }
        let (ended, touched) = observed_run(bench.input(), matches!(failing, Failing::Sweep));
        assert!(ended.errors.is_empty(), "{failing:?}: {:?}", ended.errors);
        let events = ended.journal.events();
        let condition = match failing {
            Failing::Nothing => {
                assert_eq!(ended.stop, Some(Stop::Signal), "{failing:?}");
                assert_eq!(
                    touched[..3],
                    [Touch::Sweep, Touch::Ended(trial.clone()), Touch::Start],
                    "{failing:?}"
                );
                let swept = swept_ok(events).unwrap();
                let end = trial_end(events, &trial).unwrap();
                assert!(swept < end, "{failing:?}: sweep {swept:?}, end {end:?}");
                continue;
            }
            Failing::Preflight => {
                assert_eq!(touched, [], "{failing:?}");
                DeadEndCondition::Preflight
            }
            Failing::Sweep => {
                assert_eq!(touched, [Touch::Sweep], "{failing:?}");
                DeadEndCondition::Containment
            }
        };
        assert_eq!(
            stopped_dead_end(&ended).0.condition,
            condition,
            "{failing:?}"
        );
        assert_eq!(trial_end(events, &trial), None, "{failing:?}");
        let before = events.len();
        drop(ended);

        bench.sim.fail_check("root", "");
        let (retry, touched) = observed_run(bench.input(), false);
        assert!(retry.errors.is_empty(), "{failing:?}: {:?}", retry.errors);
        assert_eq!(
            stopped_dead_end(&retry).0.condition,
            condition,
            "{failing:?}"
        );
        assert_eq!(
            touched,
            [Touch::Sweep, Touch::Ended(trial.clone())],
            "{failing:?}"
        );
        let events = &retry.journal.events()[before..];
        let swept = swept_ok(events).unwrap();
        let end = trial_end(events, &trial).unwrap();
        assert!(swept < end, "{failing:?}: sweep {swept:?}, end {end:?}");
        if let Some(shutdown) = events
            .iter()
            .find(|event| event.kind() == EventKind::Shutdown)
        {
            assert!(end < shutdown.seq, "{failing:?}");
        }
    }
}
