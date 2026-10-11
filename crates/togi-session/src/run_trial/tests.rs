use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use togi_config::Config;
use togi_journal::fs::{Fault, Op};
use togi_journal::{DeadEndCondition, Outcome as Verdict, Payload, ShutdownReason, TrialEnd};
use togi_machine::{
    Backend, BankType, BootId, CancellationToken, Clock, CpuId, Error, Kernel, KernelCursor,
    KernelRead, Machine, Mce, Partial, PmTable, PstoreRecord, Regime, Reporter, ResetReason,
    Running, Signal, Started, Stopped, TrialConditions, TrialFailure, TrialResult, TrialSpec,
    Trials,
};
use togi_sim::faults::{HookPhase, SmuOp, SmuOperation};

use crate::session::{Control, Session, Stop};
use crate::testing::{Bench, core, kinds_of, run_plan, search_plan, swept};

fn failure() -> Error {
    Error::Other("injected runner failure".into())
}

struct KernelLog {
    mces: Vec<Mce>,
    reads: usize,
    loss: Option<(usize, fn() -> Error)>,
    clock: Box<dyn Clock>,
}

type Log = Rc<RefCell<KernelLog>>;

fn add_mce(log: &Log, corrected: bool, label: &str) {
    stamp_mce(log, corrected, label, Duration::ZERO);
}

fn add_mce_before(log: &Log, corrected: bool, label: &str) {
    stamp_mce(log, corrected, label, Duration::from_nanos(1));
}

fn stamp_mce(log: &Log, corrected: bool, label: &str, ago: Duration) {
    let mut log = log.borrow_mut();
    let mce = Mce {
        cpu: CpuId(0),
        core: Some(core(0)),
        bank: 0,
        bank_type: BankType::LoadStore,
        corrected,
        time: log.clock.now(),
        monotonic: log.clock.monotonic().saturating_sub(ago),
        lines: vec![label.to_owned()],
    };
    log.mces.push(mce);
}

fn lose_read(log: &Log, nth: usize, error: fn() -> Error) {
    let mut log = log.borrow_mut();
    log.loss = Some((log.reads + nth, error));
}

struct LogKernel {
    log: Log,
    inner: Box<dyn Kernel>,
}

impl Kernel for LogKernel {
    fn mces(&mut self, _boot: &BootId, since: Duration) -> Result<Vec<Mce>, Error> {
        Ok(self
            .log
            .borrow()
            .mces
            .iter()
            .filter(|mce| mce.monotonic >= since)
            .cloned()
            .collect())
    }

    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<Vec<Mce>>> {
        let mut log = self.log.borrow_mut();
        log.reads += 1;
        let start = match after {
            None => Some(0),
            Some(cursor) => cursor
                .as_str()
                .strip_prefix(boot.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .and_then(|index| index.parse::<usize>().ok())
                .filter(|&index| index <= log.mces.len()),
        };
        let Some(start) = start else {
            return Err(Partial {
                value: Vec::new(),
                error: Error::CursorMissing,
            });
        };
        let mces = log.mces[start..].to_vec();
        if let Some((nth, error)) = log.loss
            && nth == log.reads
        {
            log.loss = None;
            return Err(Partial {
                value: mces,
                error: error(),
            });
        }
        Ok(KernelRead {
            mces,
            cursor: KernelCursor::from(format!("{boot}/{}", log.mces.len())),
        })
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

struct RetryClock {
    inner: Box<dyn Clock>,
    waits: Rc<RefCell<Vec<Duration>>>,
    fail_next: Rc<Cell<Option<fn() -> Error>>>,
}

impl Clock for RetryClock {
    fn now(&self) -> jiff::Timestamp {
        self.inner.now()
    }

    fn monotonic(&self) -> Duration {
        self.inner.monotonic()
    }

    fn sleep(&mut self, cancel: &CancellationToken, duration: Duration) -> Result<(), Error> {
        self.waits.borrow_mut().push(duration);
        match self.fail_next.take() {
            Some(error) => Err(error()),
            None => self.inner.sleep(cancel, duration),
        }
    }
}

/// What the rigged trials change about a trial the simulator runs.
#[derive(Default)]
struct Rigging {
    result: Option<TrialResult>,
    wait_error: Option<fn() -> Error>,
    stop_error: Option<fn() -> Error>,
    cancel: Option<CancellationToken>,
    on_start: Option<Rc<dyn Fn()>>,
    samples: Option<Vec<togi_machine::TrialConditions>>,
}

type Rigs = Rc<RefCell<Rigging>>;

struct Rigged {
    inner: Box<dyn Trials>,
    rigs: Rigs,
}

struct RiggedRunning {
    inner: Box<dyn Running>,
    rigs: Rigs,
}

impl Trials for Rigged {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<togi_machine::Swept, Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        if let Some(hook) = self.rigs.borrow().on_start.clone() {
            hook();
        }
        let inner = self.inner.start(cancel, spec, ready)?;
        Ok(Box::new(RiggedRunning {
            inner,
            rigs: self.rigs.clone(),
        }))
    }

    fn ended(&mut self, id: &str) -> Result<(), Error> {
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        match self.rigs.borrow().samples.clone() {
            Some(samples) => Box::new(samples.into_iter()),
            None => self.inner.samples(id),
        }
    }
}

impl Running for RiggedRunning {
    fn started(&self) -> Started {
        self.inner.started()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        let simulated = self.inner.wait(cancel, report);
        let rigs = self.rigs.borrow();
        if let Some(token) = &rigs.cancel {
            token.cancel();
        }
        let value = match (&rigs.result, simulated) {
            (Some(result), _) => result.clone(),
            (None, Ok(result)) => result,
            (None, Err(partial)) => return Err(partial),
        };
        match rigs.wait_error {
            Some(error) => Err(Partial {
                value,
                error: error(),
            }),
            None => Ok(value),
        }
    }

    fn stop(self: Box<Self>) -> Result<Stopped, Error> {
        let stop_error = self.rigs.borrow().stop_error;
        let stopped = self.inner.stop();
        match stop_error {
            Some(error) => Err(error()),
            None => stopped,
        }
    }
}

struct Rigged3 {
    session: Session,
    log: Log,
    rigs: Rigs,
    waits: Rc<RefCell<Vec<Duration>>>,
    fail_sleep: Rc<Cell<Option<fn() -> Error>>>,
    bench: Bench,
}

fn rigged(bench: Bench) -> Rigged3 {
    let mut input = bench.input();
    let machine = std::mem::replace(&mut input.machine, bench.sim.seams());
    let Machine {
        clock,
        smu,
        host,
        trials,
        kernel,
    } = machine;
    let log: Log = Rc::new(RefCell::new(KernelLog {
        mces: Vec::new(),
        reads: 0,
        loss: None,
        clock: bench.sim.seams().clock,
    }));
    let rigs = Rigs::default();
    let waits = Rc::new(RefCell::new(Vec::new()));
    let fail_sleep = Rc::new(Cell::new(None));
    input.machine = Machine {
        clock: Box::new(RetryClock {
            inner: clock,
            waits: waits.clone(),
            fail_next: fail_sleep.clone(),
        }),
        smu,
        host,
        trials: Box::new(Rigged {
            inner: trials,
            rigs: rigs.clone(),
        }),
        kernel: Box::new(LogKernel {
            log: log.clone(),
            inner: kernel,
        }),
    };
    let mut session = match Session::open(input) {
        Ok(session) => session,
        Err(ended) => panic!("open: {:?}", ended.errors),
    };
    assert_eq!(session.start().unwrap(), Control::Continue);
    assert_eq!(session.sweep().unwrap(), Control::Continue);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    Rigged3 {
        session,
        log,
        rigs,
        waits,
        fail_sleep,
        bench,
    }
}

fn after_one_inconclusive(config: &str) -> Bench {
    let mut bench = Bench::new(2);
    bench.config = Config::parse(config).unwrap();
    bench
}

fn ends(session: &Session) -> Vec<TrialEnd> {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::TrialEnd(end) => Some(end.clone()),
            _ => None,
        })
        .collect()
}

fn retries(session: &Session) -> Vec<(u8, u32)> {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::BackendRetry(retry) => Some((retry.attempt, retry.wait_s)),
            _ => None,
        })
        .collect()
}

fn kind_count(session: &Session, kind: &str) -> usize {
    kinds_of(session.appender())
        .into_iter()
        .filter(|found| *found == kind)
        .count()
}

fn trial(rig: &mut Rigged3) -> crate::session::Outcome {
    let plan = search_plan(&rig.session, core(0), Regime::R1);
    run_plan(&mut rig.session, plan)
}

fn dead_end_condition(control: &Control) -> Option<DeadEndCondition> {
    match control {
        Control::Stop(Stop::DeadEnd { dead_end, .. }) => Some(dead_end.condition),
        _ => None,
    }
}

const IMMEDIATE: &str = r#"{"dead_ends":{"inconclusive_in_a_row":1}}"#;

#[test]
fn a_clean_trial_alone_sets_its_core_runs_and_resets_it() {
    let mut bench = Bench::new(2);
    bench.config = Config::parse(r#"{"start_offsets":{"0":-5}}"#).unwrap();
    let mut session = swept(&bench);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    let plan = search_plan(&session, core(0), Regime::R1);
    assert_eq!(run_plan(&mut session, plan).unwrap(), Control::Continue);
    let kinds: Vec<_> = kinds_of(session.appender())
        .into_iter()
        .skip_while(|kind| *kind != "trial.intent")
        .collect();
    assert_eq!(kinds.first(), Some(&"trial.intent"));
    let start = kinds
        .iter()
        .position(|kind| *kind == "trial.start")
        .unwrap();
    let before = &kinds[..start];
    assert_eq!(
        before.iter().filter(|kind| **kind == "smu.write").count(),
        1,
        "{before:?}"
    );
    let after = &kinds[start..];
    assert_eq!(
        after.iter().filter(|kind| **kind == "smu.write").count(),
        1,
        "{after:?}"
    );
    assert_eq!(kinds.last(), Some(&"trial.end"));
    let end = &ends(&session)[0];
    assert_eq!(end.outcome, Verdict::Pass);
    assert_eq!(end.core, None);
    crate::testing::assert_fold_consistent(session.appender());
}

#[test]
fn a_trial_start_cites_its_intent_and_the_readbacks_that_proved_the_profile() {
    let bench = Bench::new(2);
    let mut session = swept(&bench);
    assert_eq!(session.begin().unwrap(), Control::Continue);
    let plan = search_plan(&session, core(0), Regime::R1);
    run_plan(&mut session, plan).unwrap();
    let events = session.appender().journal().events();
    let start = events
        .iter()
        .find(|event| matches!(event.payload, Payload::TrialStart(_)))
        .unwrap();
    let intent = events
        .iter()
        .find(|event| matches!(event.payload, Payload::TrialIntent(_)))
        .unwrap();
    assert_eq!(start.cause[0], intent.seq);
    assert!(start.cause[1..].iter().all(|seq| {
        events
            .iter()
            .any(|event| event.seq == *seq && matches!(event.payload, Payload::SmuReadback(_)))
    }));
    assert!(start.cause.len() > 1);
}

#[test]
fn a_trial_waits_at_one_five_and_thirty_minutes_after_inconclusive_trials_and_resumes_a_wait() {
    let mut rig = rigged(after_one_inconclusive(IMMEDIATE));
    rig.bench.sim.fail_setup(5);
    assert_eq!(trial(&mut rig).unwrap(), Control::Continue);
    assert_eq!(ends(&rig.session)[0].outcome, Verdict::Inconclusive);
    rig.fail_sleep.set(Some(|| Error::Crashed));
    let crashed = trial(&mut rig).unwrap_err();
    assert!(crate::execute::is_crash(&crashed), "{crashed:?}");
    assert_eq!(retries(&rig.session), [(1, 60)]);
    assert_eq!(trial(&mut rig).unwrap(), Control::Continue);
    assert_eq!(retries(&rig.session), [(1, 60)]);
    assert_eq!(trial(&mut rig).unwrap(), Control::Continue);
    let last = trial(&mut rig).unwrap();
    assert_eq!(
        dead_end_condition(&last),
        Some(DeadEndCondition::NoEvidence)
    );
    assert_eq!(retries(&rig.session), [(1, 60), (2, 300), (3, 1800)]);
    let minute = Duration::from_secs(60);
    assert_eq!(
        *rig.waits.borrow(),
        [minute, minute, 5 * minute, 30 * minute]
    );
}

#[test]
fn a_missing_backend_dead_ends_at_once_with_no_retry() {
    let mut rig = rigged(Bench::new(2));
    rig.bench.sim.miss_backend(Backend::Mprime);
    let control = trial(&mut rig).unwrap();
    assert_eq!(
        dead_end_condition(&control),
        Some(DeadEndCondition::NoEvidence)
    );
    let ends = ends(&rig.session);
    assert_eq!(ends.len(), 1);
    assert!(ends[0].backend_missing);
    assert_eq!(kind_count(&rig.session, "backend.retry"), 0);
}

#[test]
fn four_backend_setup_failures_wait_then_dead_end_without_evidence() {
    let mut rig = rigged(after_one_inconclusive(IMMEDIATE));
    rig.bench.sim.fail_setup(4);
    let mut last = Control::Continue;
    for _ in 0..4 {
        last = trial(&mut rig).unwrap();
    }
    assert_eq!(
        dead_end_condition(&last),
        Some(DeadEndCondition::NoEvidence)
    );
    let waits: Vec<u32> = retries(&rig.session)
        .into_iter()
        .map(|(_, wait)| wait)
        .collect();
    assert_eq!(waits, [60, 300, 1800]);
    let ends = ends(&rig.session);
    assert_eq!(ends.len(), 4);
    assert!(
        ends.iter()
            .all(|end| end.outcome == Verdict::Inconclusive && end.reason.is_some())
    );
}

#[test]
fn a_runner_failure_ends_an_inconclusive_trial_that_counts_toward_the_backend() {
    let mut rig = rigged(after_one_inconclusive(IMMEDIATE));
    rig.rigs.borrow_mut().wait_error = Some(failure);
    for attempt in 1..=3 {
        trial(&mut rig).unwrap();
        let end = ends(&rig.session).pop().unwrap();
        assert_eq!(end.outcome, Verdict::Inconclusive);
        assert!(!end.interrupted);
        assert_eq!(
            end.reason.as_deref(),
            Some("trial runner failed: injected runner failure")
        );
        let streak = rig
            .session
            .appender()
            .recovery()
            .streak(Backend::Mprime)
            .len();
        assert_eq!(streak, attempt);
    }
    let last = trial(&mut rig).unwrap();
    assert_eq!(
        dead_end_condition(&last),
        Some(DeadEndCondition::NoEvidence)
    );
}

#[test]
fn a_cancelled_wait_stays_interrupted_and_does_not_count_toward_the_backend() {
    let mut rig = rigged(Bench::new(2));
    {
        let mut rigs = rig.rigs.borrow_mut();
        rigs.cancel = Some(rig.session.cancel.clone());
        rigs.wait_error = Some(|| Error::Cancelled);
        rigs.result = Some(TrialResult {
            ran: Duration::from_secs(85),
            ..TrialResult::default()
        });
    }
    trial(&mut rig).unwrap();
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(end.outcome, Verdict::Inconclusive);
    assert!(end.interrupted);
    assert_eq!(end.duration_s, 85);
    assert!(end.reason.unwrap().ends_with("stopped by signal"));
    assert!(
        rig.session
            .appender()
            .recovery()
            .streak(Backend::Mprime)
            .is_empty()
    );
}

#[test]
fn a_cancelled_backend_wait_stops_the_run_before_another_trial() {
    let mut rig = rigged(after_one_inconclusive(IMMEDIATE));
    rig.rigs.borrow_mut().wait_error = Some(failure);
    trial(&mut rig).unwrap();
    rig.rigs.borrow_mut().wait_error = None;
    let intents = kind_count(&rig.session, "trial.intent");
    rig.fail_sleep.set(Some(|| Error::Cancelled));
    assert_eq!(trial(&mut rig).unwrap(), Control::Stop(Stop::Signal));
    assert_eq!(kind_count(&rig.session, "trial.intent"), intents);
    assert_eq!(retries(&rig.session), [(1, 60)]);
    assert_eq!(
        rig.session
            .shutdown
            .as_ref()
            .map(|shutdown| shutdown.reason),
        Some(ShutdownReason::Signal)
    );
}

#[test]
fn a_backend_retry_that_cannot_be_journaled_prevents_the_wait_and_the_trial() {
    let mut rig = rigged(after_one_inconclusive(IMMEDIATE));
    rig.rigs.borrow_mut().wait_error = Some(failure);
    trial(&mut rig).unwrap();
    let intents = kind_count(&rig.session, "trial.intent");
    rig.bench.mem.inject(Fault::before(Op::Write, 1));
    assert!(trial(&mut rig).is_err());
    assert!(rig.waits.borrow().is_empty());
    assert_eq!(kind_count(&rig.session, "trial.intent"), intents);
    assert_eq!(kind_count(&rig.session, "backend.retry"), 0);
}

fn full_run() -> Duration {
    Duration::from_secs(90)
}

fn failing_with(signal: Signal) -> TrialResult {
    TrialResult {
        ran: full_run(),
        failure: Some(TrialFailure {
            signal,
            core: core(0),
        }),
        ..TrialResult::default()
    }
}

fn mce_when_the_trial_starts(rig: &Rigged3, corrected: bool) {
    let log = rig.log.clone();
    rig.rigs.borrow_mut().on_start =
        Some(Rc::new(move || add_mce(&log, corrected, "during trial")));
}

fn between_trial_labels(session: &Session) -> Vec<(bool, Vec<String>)> {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::Mce(mce) => {
                Some((mce.between_trials && mce.trial.is_none(), mce.lines.clone()))
            }
            _ => None,
        })
        .collect()
}

fn cites_trial_mce(session: &Session, end: &TrialEnd, corrected: bool) -> bool {
    let events = session.appender().journal().events();
    let cause = &events.last().unwrap().cause;
    cause.iter().any(|seq| {
        events.iter().any(|event| {
            event.seq == *seq
                && matches!(&event.payload, Payload::Mce(mce)
                    if mce.trial.as_ref() == Some(&end.trial)
                        && !mce.between_trials
                        && mce.corrected == corrected)
        })
    })
}

#[test]
fn profile_writes_and_retry_waits_are_outside_every_trial_window() {
    let mut bench = Bench::new(2);
    bench.config = Config::parse(r#"{"start_offsets":{"0":-1}}"#).unwrap();
    let mut rig = rigged(bench);
    rig.rigs.borrow_mut().result = Some(TrialResult {
        ran: Duration::from_secs(1),
        ..TrialResult::default()
    });
    let mut clock = rig.bench.sim.seams().clock;
    clock
        .sleep(&CancellationToken::new(), Duration::from_secs(1))
        .unwrap();
    let log = rig.log.clone();
    rig.bench
        .sim
        .interrupt_smu(move |operation: &SmuOperation| {
            if operation.phase == HookPhase::After
                && operation.op == SmuOp::Set
                && operation.offset == -1
            {
                add_mce_before(&log, true, "profile write");
            }
            Ok(())
        });
    trial(&mut rig).unwrap();
    clock
        .sleep(&CancellationToken::new(), Duration::from_secs(60))
        .unwrap();
    add_mce_before(&rig.log, true, "retry wait");
    clock
        .sleep(&CancellationToken::new(), Duration::from_secs(1))
        .unwrap();
    trial(&mut rig).unwrap();
    let outcomes: Vec<_> = ends(&rig.session).iter().map(|end| end.outcome).collect();
    assert_eq!(outcomes, [Verdict::Inconclusive, Verdict::Inconclusive]);
    let labels: Vec<String> = between_trial_labels(&rig.session)
        .into_iter()
        .map(|(between, lines)| {
            assert!(between, "{lines:?} fed a trial");
            lines.concat()
        })
        .collect();
    assert_eq!(labels, ["profile write", "retry wait", "profile write"]);
    assert_eq!(kind_count(&rig.session, "failure"), 0);
}

#[test]
fn a_lost_start_boundary_keeps_its_error_below_stronger_evidence() {
    type Loss = fn() -> Error;
    let losses: [Loss; 2] = [failure, || Error::CursorMissing];
    for loss in losses {
        for signal in [
            None,
            Some(Signal::ComputationError),
            Some(Signal::CorrectedMce),
        ] {
            let mut rig = rigged(Bench::new(2));
            lose_read(&rig.log, 1, loss);
            match signal {
                Some(Signal::CorrectedMce) => mce_when_the_trial_starts(&rig, true),
                Some(signal) => rig.rigs.borrow_mut().result = Some(failing_with(signal)),
                None => {}
            }
            trial(&mut rig).unwrap();
            let end = ends(&rig.session).pop().unwrap();
            let want = if signal.is_some() {
                Verdict::Failure
            } else {
                Verdict::Inconclusive
            };
            assert_eq!(end.outcome, want, "{signal:?}");
            assert_eq!(end.signal, signal, "{signal:?}");
            assert!(end.kernel_error.is_some(), "{signal:?}");
            if signal.is_none() {
                assert_eq!(trial(&mut rig).unwrap(), Control::Continue);
                assert_eq!(ends(&rig.session).pop().unwrap().outcome, Verdict::Pass);
            }
        }
    }
}

#[test]
fn a_machine_check_the_reset_provokes_is_evidence_of_the_trial() {
    let mut rig = rigged(Bench::new(2));
    let armed = Rc::new(Cell::new(false));
    let flag = armed.clone();
    rig.rigs.borrow_mut().on_start = Some(Rc::new(move || flag.set(true)));
    let log = rig.log.clone();
    rig.bench
        .sim
        .interrupt_smu(move |operation: &SmuOperation| {
            if armed.get() && operation.phase == HookPhase::After && operation.op == SmuOp::Set {
                add_mce(&log, true, "teardown");
            }
            Ok(())
        });
    trial(&mut rig).unwrap();
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(end.outcome, Verdict::Failure);
    assert_eq!(end.signal, Some(Signal::CorrectedMce));
    assert!(cites_trial_mce(&rig.session, &end, true));
}

#[test]
fn a_partial_kernel_read_with_a_machine_check_outranks_the_read_error() {
    let mut rig = rigged(Bench::new(2));
    mce_when_the_trial_starts(&rig, true);
    lose_read(&rig.log, 2, failure);
    trial(&mut rig).unwrap();
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(end.outcome, Verdict::Failure);
    assert_eq!(end.signal, Some(Signal::CorrectedMce));
    assert!(end.kernel_error.is_some());
}

#[test]
fn an_uncorrected_machine_check_rejects_a_full_duration_pass() {
    let mut rig = rigged(Bench::new(2));
    mce_when_the_trial_starts(&rig, false);
    trial(&mut rig).unwrap();
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(end.outcome, Verdict::Failure);
    assert_eq!(end.signal, Some(Signal::UncorrectedMce));
    assert_eq!(end.core, None);
    assert!(cites_trial_mce(&rig.session, &end, false));
}

#[test]
fn a_crash_at_a_kernel_boundary_never_invents_a_pass() {
    let mut rig = rigged(Bench::new(2));
    lose_read(&rig.log, 1, || Error::Crashed);
    let error = trial(&mut rig).unwrap_err();
    assert!(crate::execute::is_crash(&error), "{error:?}");
    assert_eq!(kind_count(&rig.session, "trial.intent"), 0);

    let mut rig = rigged(Bench::new(2));
    lose_read(&rig.log, 2, || Error::Crashed);
    let error = trial(&mut rig).unwrap_err();
    assert!(crate::execute::is_crash(&error), "{error:?}");
    assert_eq!(kind_count(&rig.session, "trial.start"), 1);
    assert!(ends(&rig.session).is_empty());

    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().result = Some(failing_with(Signal::ComputationError));
    lose_read(&rig.log, 2, || Error::Crashed);
    let error = trial(&mut rig).unwrap_err();
    assert!(crate::execute::is_crash(&error), "{error:?}");
    let ends = ends(&rig.session);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].outcome, Verdict::Failure);
    assert_eq!(ends[0].signal, Some(Signal::ComputationError));
    assert!(ends[0].interrupted);
    assert!(ends[0].reason.as_deref().unwrap().contains("crashed"));
}

#[test]
fn a_missing_current_boot_leaves_what_the_trial_itself_showed() {
    let cases: [(&str, Option<Signal>, bool, Verdict, Option<Signal>); 5] = [
        ("missing", None, false, Verdict::Inconclusive, None),
        (
            "computation error",
            Some(Signal::ComputationError),
            false,
            Verdict::Failure,
            Some(Signal::ComputationError),
        ),
        (
            "stall",
            Some(Signal::Stall),
            false,
            Verdict::Failure,
            Some(Signal::Stall),
        ),
        (
            "early exit",
            Some(Signal::UnexpectedExit),
            false,
            Verdict::Failure,
            Some(Signal::UnexpectedExit),
        ),
        (
            "mce",
            None,
            true,
            Verdict::Failure,
            Some(Signal::CorrectedMce),
        ),
    ];
    for (name, signal, mce, outcome, want) in cases {
        let mut rig = rigged(Bench::new(2));
        lose_read(&rig.log, 2, || Error::BootMissing);
        if let Some(signal) = signal {
            rig.rigs.borrow_mut().result = Some(failing_with(signal));
        }
        if mce {
            mce_when_the_trial_starts(&rig, true);
        }
        trial(&mut rig).unwrap();
        let end = ends(&rig.session).pop().unwrap();
        assert_eq!((end.outcome, end.signal), (outcome, want), "{name}");
    }
}

#[test]
fn evidence_survives_a_runner_error_and_the_error_stays_in_the_reason() {
    let cases: [(&str, Option<TrialResult>, bool, Verdict, Option<Signal>); 3] = [
        (
            "signal",
            Some(failing_with(Signal::ComputationError)),
            false,
            Verdict::Failure,
            Some(Signal::ComputationError),
        ),
        (
            "escape",
            Some(TrialResult {
                ran: full_run(),
                escaped: vec![CpuId(99)],
                ..TrialResult::default()
            }),
            false,
            Verdict::Inconclusive,
            None,
        ),
        (
            "mce",
            None,
            true,
            Verdict::Failure,
            Some(Signal::CorrectedMce),
        ),
    ];
    for (name, result, mce, outcome, want) in cases {
        let mut rig = rigged(Bench::new(2));
        {
            let mut rigs = rig.rigs.borrow_mut();
            rigs.wait_error = Some(|| Error::Other("cleanup failed".into()));
            rigs.result = result.or_else(|| {
                Some(TrialResult {
                    ran: full_run(),
                    ..TrialResult::default()
                })
            });
        }
        if mce {
            mce_when_the_trial_starts(&rig, true);
            lose_read(&rig.log, 2, failure);
        }
        trial(&mut rig).unwrap();
        let end = ends(&rig.session).pop().unwrap();
        assert_eq!((end.outcome, end.signal), (outcome, want), "{name}");
        assert!(end.reason.unwrap().contains("cleanup failed"), "{name}");
    }
}

#[test]
fn a_backend_that_exits_during_setup_keeps_the_machine_check_it_logged() {
    let mut rig = rigged(Bench::new(2));
    rig.bench.sim.fail_setup(1);
    mce_when_the_trial_starts(&rig, true);
    trial(&mut rig).unwrap();
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(end.outcome, Verdict::Failure);
    assert_eq!(end.signal, Some(Signal::CorrectedMce));
    assert!(cites_trial_mce(&rig.session, &end, true));
}

fn request_samples(count: usize) -> Vec<TrialConditions> {
    (0..count)
        .map(|i| {
            let mut table = PmTable::default();
            table.voltage_request_v[0] = 1.25;
            TrialConditions {
                elapsed_ms: 5000 + i64::try_from(i).unwrap() * 1000,
                core_mhz: [(core(0), 5000)].into_iter().collect(),
                pm_table: Some(table),
                ..TrialConditions::default()
            }
        })
        .collect()
}

#[test]
fn request_telemetry_needs_twenty_samples_after_the_warmup() {
    for (count, recorded) in [(19, false), (20, true)] {
        let mut rig = rigged(Bench::new(2));
        rig.rigs.borrow_mut().samples = Some(request_samples(count));
        trial(&mut rig).unwrap();
        let end = ends(&rig.session).pop().unwrap();
        assert_eq!(!end.voltage_requests_v.is_empty(), recorded, "{count}");
        assert_eq!(!end.ccd_mhz.is_empty(), recorded, "{count}");
    }
}
