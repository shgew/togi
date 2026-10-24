use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use test_case::test_case;

use togi_config::Config;
use togi_journal::fs::{Fault as FsFault, Op};
use togi_journal::{
    DeadEndCondition, Outcome as Verdict, Payload, ShutdownReason, TrialEnd, TrialInstance,
    TrialStart,
};
use togi_machine::{
    Backend, BankType, BootId, CancellationToken, Clock, CpuId, Error, Instance, Kernel,
    KernelCursor, KernelRead, LoadSchedule, Machine, Mce, Partial, PmTable, PstoreRecord, Regime,
    Reporter, ResetReason, Running, Signal, Started, Stopped, TargetOffset, TrialConditions,
    TrialFailure, TrialResult, TrialSpec, Trials,
};
use togi_sim::faults::{HookPhase, SmuOp, SmuOperation};

use togi_machine::Condition;
use togi_sim::model::Script;

use crate::error::{Flow, SessionError};
use crate::execute::TunerValues;
use crate::session::{Control, Fault, Session, Stop};
use crate::testing::{Bench, core, kinds_of, run_plan, search_plan, swept};

fn failure() -> Error {
    Error::Other("injected runner failure".into())
}

struct KernelLog {
    mces: Vec<Mce>,
    faults: Vec<togi_machine::ProcessFault>,
    reads: usize,
    loss: Option<(usize, fn() -> Error)>,
    answers: Vec<(usize, Answer)>,
    clock: Box<dyn Clock>,
}

/// A read answered with exactly this, whatever the log holds: a fresh machine check labelled `mce`, if any, and
/// `error`.
struct Answer {
    mce: Option<&'static str>,
    error: fn() -> Error,
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
    let mce = mce_now(&*log.clock, corrected, label, ago);
    log.mces.push(mce);
}

fn mce_now(clock: &dyn Clock, corrected: bool, label: &str, ago: Duration) -> Mce {
    Mce {
        cpu: CpuId::from(0),
        core: Some(core(0)),
        bank: 0,
        bank_type: BankType::LoadStore,
        corrected,
        time: clock.now(),
        monotonic: clock.monotonic().unwrap().saturating_sub(ago),
        lines: vec![label.to_owned()],
    }
}

fn lose_read(log: &Log, nth: usize, error: fn() -> Error) {
    let mut log = log.borrow_mut();
    log.loss = Some((log.reads + nth, error));
}

/// Answers the `nth` read from now with `answer`.
fn answer_read(log: &Log, nth: usize, answer: Answer) {
    let mut log = log.borrow_mut();
    let at = log.reads + nth;
    log.answers.push((at, answer));
}

struct LogKernel {
    log: Log,
    inner: Box<dyn Kernel>,
}

impl Kernel for LogKernel {
    fn process_threads(&mut self, pid: u32) -> Result<Vec<u32>, Error> {
        Ok(if pid == std::process::id() {
            vec![pid, 421]
        } else if pid == 991 {
            vec![pid, 422]
        } else {
            vec![pid]
        })
    }
    fn mces(&mut self, _boot: &BootId, since: Duration) -> Result<Vec<Mce>, Partial<Vec<Mce>>> {
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
    ) -> Result<KernelRead, Partial<KernelRead>> {
        let mut log = self.log.borrow_mut();
        log.reads += 1;
        let reads = log.reads;
        if let Some(at) = log.answers.iter().position(|(nth, _)| *nth == reads) {
            let (_, answer) = log.answers.swap_remove(at);
            let value = answer
                .mce
                .map(|label| mce_now(&*log.clock, true, label, Duration::ZERO))
                .into_iter()
                .collect();
            return Err(Partial {
                value: KernelRead {
                    mces: value,
                    faults: log.faults.clone(),
                    ..KernelRead::default()
                },
                error: (answer.error)(),
            });
        }
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
                value: KernelRead::default(),
                error: Error::CursorMissing,
            });
        };
        let mces = log.mces[start..].to_vec();
        if let Some((nth, error)) = log.loss
            && nth == log.reads
        {
            log.loss = None;
            return Err(Partial {
                value: KernelRead {
                    mces,
                    faults: log.faults.clone(),
                    ..KernelRead::default()
                },
                error: error(),
            });
        }
        Ok(KernelRead {
            mces,
            faults: log.faults.clone(),
            errors: Vec::new(),
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

type Pending = Rc<Cell<Option<fn() -> Error>>>;

struct RetryClock {
    inner: Box<dyn Clock>,
    waits: Rc<RefCell<Vec<Duration>>>,
    fail_next: Pending,
}

impl Clock for RetryClock {
    fn now(&self) -> jiff::Timestamp {
        self.inner.now()
    }

    fn monotonic(&self) -> Result<Duration, togi_machine::Error> {
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
    result_for_spec: Option<fn(&TrialSpec) -> TrialResult>,
    reported_failure: Option<TrialFailure>,
    stop_error: Option<fn() -> Error>,
    cancel: Option<CancellationToken>,
    on_start: Option<Rc<dyn Fn()>>,
    samples: Option<Vec<togi_machine::TrialConditions>>,
    samples_file: Option<(Vec<u8>, bool)>,
    instances: Option<Vec<Instance>>,
    pid: Option<u32>,
    schedule: Option<LoadSchedule>,
    /// Makes the start fail with this error and hand back no proof that nothing runs.
    start_error: Option<fn() -> Error>,
    /// Runs once the workload's wait has returned, before it is stopped.
    after_wait: Option<Rc<dyn Fn()>>,
    /// The specs the trials were started with, oldest first.
    specs: Vec<TrialSpec>,
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
        if let Some(error) = self.rigs.borrow().start_error {
            return Err(Partial {
                value: None,
                error: error(),
            });
        }
        self.rigs.borrow_mut().specs.push(spec.clone());
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
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
        let rigs = self.rigs.borrow();
        if let Some((bytes, compressed)) = &rigs.samples_file {
            let reader: Box<dyn std::io::Read> = if *compressed {
                Box::new(flate2::read::MultiGzDecoder::new(std::io::Cursor::new(
                    bytes.clone(),
                )))
            } else {
                Box::new(std::io::Cursor::new(bytes.clone()))
            };
            return Box::new(togi_machine::ReadSamples::from_reader(reader));
        }
        match rigs.samples.clone() {
            Some(samples) => Box::new(samples.into_iter().map(Ok)),
            None => self.inner.samples(id),
        }
    }
}

impl Running for RiggedRunning {
    fn started(&self) -> Started {
        let mut started = self.inner.started();
        if let Some(pid) = self.rigs.borrow().pid {
            started.pid = pid;
        }
        if let Some(instances) = self.rigs.borrow().instances.clone() {
            started.instances = instances;
        }
        if let Some(schedule) = self.rigs.borrow().schedule {
            started.schedule = Some(schedule);
        }
        started
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        let simulated = self.inner.wait(cancel, report);
        if let Some(hook) = self.rigs.borrow().after_wait.clone() {
            hook();
        }
        let rigs = self.rigs.borrow();
        if let Some(failure) = rigs.reported_failure {
            report.signal(
                failure.core,
                failure.signal,
                failure.pid.map(std::num::NonZeroU32::get),
                "backend reported before interruption",
            );
        }
        if let Some(token) = &rigs.cancel {
            token.cancel();
        }
        let value = match (&rigs.result, simulated) {
            (Some(result), _) => result.clone(),
            (None, Ok(result)) => result,
            (None, Err(partial)) => return Err(partial),
        };
        let value = rigs
            .result_for_spec
            .map_or(value, |result| result(rigs.specs.last().unwrap()));
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
    session: Session<'static>,
    log: Log,
    rigs: Rigs,
    waits: Rc<RefCell<Vec<Duration>>>,
    fail_sleep: Pending,
    bench: Bench,
}

fn rig_input(
    bench: &Bench,
) -> (
    crate::session::RunInput<'static>,
    Log,
    Rigs,
    Rc<RefCell<Vec<Duration>>>,
    Pending,
) {
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
        faults: Vec::new(),
        reads: 0,
        loss: None,
        answers: Vec::new(),
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
    (input, log, rigs, waits, fail_sleep)
}

fn rigged(bench: Bench) -> Rigged3 {
    let (input, log, rigs, waits, fail_sleep) = rig_input(&bench);
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
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end.clone())
            } else {
                None
            }
        })
        .collect()
}

fn retries(session: &Session) -> Vec<(u8, u32)> {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::BackendRetry(retry) = &event.payload {
                Some((retry.attempt, retry.wait_s))
            } else {
                None
            }
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
    if let Control::Stop(Stop::DeadEnd { dead_end, .. }) = control {
        Some(dead_end.condition)
    } else {
        None
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
fn trial_end_preserves_controller_host_exclusion_for_evidence_replay() {
    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().result = Some(TrialResult {
        ran: full_run(),
        excluded: [core(0)].into_iter().collect(),
        ..TrialResult::default()
    });
    trial(&mut rig).unwrap();
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(end.outcome, Verdict::Pass);
    assert_eq!(end.excluded, vec![core(0)]);
    assert_eq!(end.evidence_outcome(), Verdict::Inconclusive);
    crate::testing::assert_fold_consistent(rig.session.appender());
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

fn trial_start(session: &Session) -> TrialStart {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialStart(start) = &event.payload {
                Some(start.clone())
            } else {
                None
            }
        })
        .unwrap()
}

#[test]
fn a_trial_start_records_the_roles_sent_to_the_trial_backend() {
    let mut rig = rigged(Bench::new(2));
    trial(&mut rig).unwrap();
    let roles = rig.rigs.borrow().specs[0].roles.clone();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].core, core(0));
    assert_eq!(roles[0].cpu, CpuId::from(0));
    assert_eq!(trial_start(&rig.session).roles, roles);
}

#[test]
fn a_joint_trial_start_records_every_instance_and_a_single_one_is_left_out() {
    let two = vec![
        Instance {
            core: core(0),
            cpus: vec![CpuId::from(0)],
            pid: 4101,
            scope: "togi-trial-0001-a.scope".into(),
        },
        Instance {
            core: core(1),
            cpus: vec![CpuId::from(2), CpuId::from(3)],
            pid: 4102,
            scope: "togi-trial-0001-b.scope".into(),
        },
    ];
    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().instances = Some(two.clone());
    trial(&mut rig).unwrap();
    let want: Vec<TrialInstance> = two
        .into_iter()
        .map(|instance| TrialInstance {
            core: instance.core,
            cpus: instance.cpus,
            pid: instance.pid,
            scope: instance.scope,
        })
        .collect();
    assert_eq!(trial_start(&rig.session).instances, want);

    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().instances = Some(vec![Instance {
        core: core(0),
        cpus: vec![CpuId::from(0)],
        pid: 4101,
        scope: "togi-trial-0001-a.scope".into(),
    }]);
    trial(&mut rig).unwrap();
    assert!(trial_start(&rig.session).instances.is_empty());
}

#[test]
fn a_trial_waits_at_one_five_and_thirty_minutes_after_inconclusive_trials_and_resumes_a_wait() {
    let mut rig = rigged(after_one_inconclusive(IMMEDIATE));
    rig.bench.sim.fail_setup(5);
    assert_eq!(trial(&mut rig).unwrap(), Control::Continue);
    assert_eq!(ends(&rig.session)[0].outcome, Verdict::Inconclusive);
    rig.fail_sleep.set(Some(|| Error::Crashed));
    let crashed = trial(&mut rig).unwrap_err();
    assert!(crashed.is_crash(), "{crashed:?}");
    assert_eq!(retries(&rig.session), [(1, 60)]);
    rig.session.lifecycle = crate::session::Lifecycle::Unswept;
    assert_eq!(rig.session.sweep().unwrap(), Control::Continue);
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
    rig.bench.mem.inject(FsFault::before(Op::Write, 1));
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
            pid: None,
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
        .filter_map(|event| {
            if let Payload::Mce(mce) = &event.payload {
                Some((mce.between_trials && mce.trial.is_none(), mce.lines.clone()))
            } else {
                None
            }
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
fn a_trials_spec_conveys_the_confirmed_profile_so_its_own_threads_can_avoid_the_deep_cores() {
    let mut bench = Bench::new(2);
    bench.config = Config::parse(r#"{"start_offsets":{"0":-1}}"#).unwrap();
    let mut rig = rigged(bench);
    trial(&mut rig).unwrap();
    let rigs = rig.rigs.borrow();
    let spec = rigs.specs.first().expect("a trial started");
    assert_eq!(spec.offsets.iter().count(), 2, "{:?}", spec.offsets);
    assert_eq!(spec.offsets.get(core(1)), Some(TargetOffset::ZERO));
    assert!(spec.offsets.get(core(0)).is_some());
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

#[test_case(failure, None; "boot missing without signal")]
#[test_case(failure, Some(Signal::ComputationError); "boot missing with computation error")]
#[test_case(failure, Some(Signal::CorrectedMce); "boot missing with corrected MCE")]
#[test_case(|| Error::CursorMissing, None; "cursor missing without signal")]
#[test_case(|| Error::CursorMissing, Some(Signal::ComputationError); "cursor missing with computation error")]
#[test_case(|| Error::CursorMissing, Some(Signal::CorrectedMce); "cursor missing with corrected MCE")]
fn a_lost_start_boundary_keeps_its_error_below_stronger_evidence(
    loss: fn() -> Error,
    signal: Option<Signal>,
) {
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

#[test_case(None; "successful reanchor")]
#[test_case(Some(|| Error::Other("reanchor failed".into())); "failed reanchor")]
fn partial_machine_check_evidence_outranks_a_failed_or_successful_reanchor(
    fresh: Option<fn() -> Error>,
) {
    let mut rig = rigged(Bench::new(2));
    answer_read(
        &rig.log,
        2,
        Answer {
            mce: Some("partial evidence"),
            error: || Error::CursorMissing,
        },
    );
    if let Some(error) = fresh {
        answer_read(&rig.log, 3, Answer { mce: None, error });
    }
    let before = rig.log.borrow().reads;
    trial(&mut rig).unwrap();
    assert_eq!(rig.log.borrow().reads - before, 3, "{fresh:?}");
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(end.outcome, Verdict::Failure, "{fresh:?}");
    assert_eq!(end.signal, Some(Signal::CorrectedMce), "{fresh:?}");
    assert!(cites_trial_mce(&rig.session, &end, true), "{fresh:?}");
    let kernel_error = end.kernel_error.unwrap();
    assert!(
        kernel_error.contains("kernel log cursor cannot be resumed"),
        "{kernel_error}"
    );
    assert_eq!(
        kernel_error.contains("reanchor failed"),
        fresh.is_some(),
        "{kernel_error}"
    );
}

#[test]
fn a_crash_on_the_fresh_read_of_a_reanchor_never_invents_a_pass() {
    let mut rig = rigged(Bench::new(2));
    answer_read(
        &rig.log,
        2,
        Answer {
            mce: None,
            error: || Error::CursorMissing,
        },
    );
    answer_read(
        &rig.log,
        3,
        Answer {
            mce: None,
            error: || Error::Crashed,
        },
    );
    let error = trial(&mut rig).unwrap_err();
    assert!(error.is_crash(), "{error:?}");
    assert_eq!(kind_count(&rig.session, "trial.start"), 1);
    assert!(ends(&rig.session).is_empty());

    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().result = Some(failing_with(Signal::ComputationError));
    answer_read(
        &rig.log,
        2,
        Answer {
            mce: None,
            error: || Error::CursorMissing,
        },
    );
    answer_read(
        &rig.log,
        3,
        Answer {
            mce: None,
            error: || Error::Crashed,
        },
    );
    let error = trial(&mut rig).unwrap_err();
    assert!(error.is_crash(), "{error:?}");
    let ends = ends(&rig.session);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].outcome, Verdict::Failure);
    assert_eq!(ends[0].signal, Some(Signal::ComputationError));
    assert!(ends[0].interrupted);
    assert!(ends[0].reason.as_deref().unwrap().contains("crashed"));
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
    assert!(error.is_crash(), "{error:?}");
    assert_eq!(kind_count(&rig.session, "trial.intent"), 0);

    let mut rig = rigged(Bench::new(2));
    lose_read(&rig.log, 2, || Error::Crashed);
    let error = trial(&mut rig).unwrap_err();
    assert!(error.is_crash(), "{error:?}");
    assert_eq!(kind_count(&rig.session, "trial.start"), 1);
    assert!(ends(&rig.session).is_empty());

    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().result = Some(failing_with(Signal::ComputationError));
    lose_read(&rig.log, 2, || Error::Crashed);
    let error = trial(&mut rig).unwrap_err();
    assert!(error.is_crash(), "{error:?}");
    let ends = ends(&rig.session);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].outcome, Verdict::Failure);
    assert_eq!(ends[0].signal, Some(Signal::ComputationError));
    assert!(ends[0].interrupted);
    assert!(ends[0].reason.as_deref().unwrap().contains("crashed"));
}

#[test_case("missing", None, false, Verdict::Inconclusive, None; "missing")]
#[test_case("computation error", Some(Signal::ComputationError), false, Verdict::Failure, Some(Signal::ComputationError); "computation error")]
#[test_case("stall", Some(Signal::Stall), false, Verdict::Failure, Some(Signal::Stall); "stall")]
#[test_case("early exit", Some(Signal::UnexpectedExit), false, Verdict::Failure, Some(Signal::UnexpectedExit); "early exit")]
#[test_case("mce", None, true, Verdict::Failure, Some(Signal::CorrectedMce); "mce")]
fn a_missing_current_boot_leaves_what_the_trial_itself_showed(
    name: &str,
    signal: Option<Signal>,
    mce: bool,
    outcome: Verdict,
    want: Option<Signal>,
) {
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

#[test_case("signal", Some(failing_with(Signal::ComputationError)), false, Verdict::Failure, Some(Signal::ComputationError); "signal")]
#[test_case("escape", Some(TrialResult {
                ran: full_run(),
                escaped: vec![CpuId::from(99)],
                ..TrialResult::default()
            }), false, Verdict::Inconclusive, None; "escape")]
#[test_case("mce", None, true, Verdict::Failure, Some(Signal::CorrectedMce); "mce")]
fn evidence_survives_a_runner_error_and_the_error_stays_in_the_reason(
    name: &str,
    result: Option<TrialResult>,
    mce: bool,
    outcome: Verdict,
    want: Option<Signal>,
) {
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

#[test_case(19, false; "19, false")]
#[test_case(20, true; "20, true")]
fn request_telemetry_needs_twenty_samples_after_the_warmup(count: usize, recorded: bool) {
    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().samples = Some(request_samples(count));
    trial(&mut rig).unwrap();
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(!end.voltage_requests_v.is_empty(), recorded, "{count}");
    assert_eq!(!end.ccd_mhz.is_empty(), recorded, "{count}");
}

#[test_case(false, "decode conditions sample", false; "compressed false failure false")]
#[test_case(false, "decode conditions sample", true; "compressed false failure true")]
#[test_case(true, "gzip decoding", false; "compressed true failure false")]
#[test_case(true, "gzip decoding", true; "compressed true failure true")]
fn unreadable_samples_warn_and_omit_all_sample_fields_without_changing_the_outcome(
    compressed: bool,
    error: &str,
    failure: bool,
) {
    use std::io::Write as _;

    let mut bytes = Vec::new();
    for sample in request_samples(20) {
        serde_json::to_writer(&mut bytes, &sample).unwrap();
        bytes.push(b'\n');
    }
    let mut malformed = bytes.clone();
    malformed.extend_from_slice(b"not json\n");
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(&bytes).unwrap();
    let mut corrupt_gzip = gzip.finish().unwrap();
    corrupt_gzip.extend_from_slice(b"not a second gzip member");

    let bytes = if compressed { corrupt_gzip } else { malformed };

    let mut rig = rigged(Bench::new(2));
    {
        let mut rigs = rig.rigs.borrow_mut();
        rigs.samples_file = Some((bytes, compressed));
        rigs.result = Some(TrialResult {
            ran: full_run(),
            tctl_max_c: Some(77),
            failure: failure.then_some(TrialFailure {
                signal: Signal::ComputationError,
                core: core(0),
                pid: None,
            }),
            ..TrialResult::default()
        });
    }
    trial(&mut rig).unwrap();
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(
        end.outcome,
        if failure {
            Verdict::Failure
        } else {
            Verdict::Pass
        }
    );
    assert_sample_fields_absent(&end);
    let events = rig.session.appender().journal().events();
    let warning = events
        .iter()
        .find_map(|event| {
            if let Payload::SessionWarning(warning) = &event.payload
                && warning.operation == "read conditions samples"
            {
                Some((event, warning))
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(warning.1.trial.as_ref(), Some(&end.trial));
    assert!(warning.1.error.contains(error), "{}", warning.1.error);
    let ended = events
        .iter()
        .find(|event| matches!(&event.payload, Payload::TrialEnd(_)))
        .unwrap();
    assert_eq!(warning.0.cause, [ended.seq]);
}

fn assert_sample_fields_absent(end: &TrialEnd) {
    let data = serde_json::to_value(end).unwrap();
    for field in [
        "tctl_max_c",
        "voltage_request_median_v",
        "voltage_request_min_v",
        "voltage_requests_v",
        "top_requesters",
        "ccd_mhz",
        "last_sample_s",
        "last_sample_tctl_c",
        "last_sample_min_mhz",
        "last_sample_max_mhz",
        "stalled_core",
        "worker_stalled_ms",
    ] {
        assert!(data.get(field).is_none(), "{field}: {data}");
    }
}

fn together_plan(session: &Session, offsets: &[i64]) -> togi_tuner::TrialPlan {
    let mut plan = search_plan(session, core(0), Regime::R1);
    let topology = session.appender().tuner().topology().unwrap();
    plan.condition = Condition::Together;
    plan.load = togi_tuner::Load::Many(session.cores());
    plan.profile = togi_tuner::Profile::new(topology, crate::testing::offsets(offsets)).unwrap();
    plan
}

fn tuned_to(plan: &togi_tuner::TrialPlan) -> TunerValues<'_> {
    TunerValues {
        profile: Some(&plan.profile),
        profile_seq: None,
        clean_cycles: 0,
        first_result: false,
    }
}

fn applied_conditions(session: &Session) -> Vec<Condition> {
    session
        .appender()
        .journal()
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::ProfileApplied(applied) = &event.payload {
                Some(applied.condition)
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn each_condition_puts_the_machine_on_its_profile_and_an_applied_one_is_not_written_again() {
    let mut rig = rigged(Bench::new(2));
    let together = together_plan(&rig.session, &[-5, -3]);
    rig.session
        .ensure_condition(&together, &tuned_to(&together))
        .unwrap();
    assert_eq!(applied_conditions(&rig.session), [Condition::Together]);
    let mut smu = rig.bench.sim.seams().smu;
    assert_eq!(smu.offset(core(0)).unwrap().0, -5);
    assert_eq!(smu.offset(core(1)).unwrap().0, -3);

    let events = rig.session.appender().journal().events().len();
    rig.session
        .ensure_condition(&together, &tuned_to(&together))
        .unwrap();
    assert_eq!(rig.session.appender().journal().events().len(), events);

    let mut alone = search_plan(&rig.session, core(0), Regime::R1);
    alone.condition = Condition::Alone;
    rig.session
        .ensure_condition(&alone, &tuned_to(&together))
        .unwrap();
    assert_eq!(smu.offset(core(0)).unwrap().0, 0);
    assert_eq!(smu.offset(core(1)).unwrap().0, 0);

    let mut parked = together_plan(&rig.session, &[-2, -2]);
    parked.condition = Condition::Parked;
    rig.session
        .ensure_condition(&parked, &tuned_to(&together))
        .unwrap();
    assert_eq!(smu.offset(core(0)).unwrap().0, -2);
    assert_eq!(
        applied_conditions(&rig.session)[1..],
        [Condition::Alone, Condition::Parked]
    );
}

#[test]
fn a_together_trial_must_carry_the_profile_the_tuner_stands_at() {
    let mut rig = rigged(Bench::new(2));
    let plan = together_plan(&rig.session, &[-5, -3]);
    let other = together_plan(&rig.session, &[-4, -3]);
    let flow = rig
        .session
        .ensure_condition(&plan, &tuned_to(&other))
        .unwrap_err();
    assert!(matches!(flow, Flow::Fatal(SessionError::PlanDisagrees)));
    assert_eq!(kind_count(&rig.session, "profile.applied"), 0);
}

#[test]
fn nothing_is_written_or_started_without_the_proof_that_nothing_runs() {
    let mut rig = rigged(Bench::new(2));
    rig.session.lifecycle.lose_stopped();
    let plan = together_plan(&rig.session, &[-5, -3]);
    let events = rig.session.appender().journal().events().len();
    let flow = rig
        .session
        .ensure_condition(&plan, &tuned_to(&plan))
        .unwrap_err();
    assert!(matches!(flow, Flow::Fatal(SessionError::Unproven)));
    let outcome = trial(&mut rig).unwrap_err();
    assert!(matches!(outcome, SessionError::Unproven));
    assert_eq!(rig.session.appender().journal().events().len(), events);
}

#[test]
fn an_unconfirmed_containment_withholds_the_proof_and_ends_the_trial_as_such() {
    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().stop_error = Some(|| Error::Containment("workload still running".into()));
    trial(&mut rig).unwrap();
    assert_eq!(rig.session.lifecycle.fault(), Some(Fault::Containment));
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(end.outcome, Verdict::Inconclusive);
    assert!(end.containment_error.is_some());
    let writes_after_start: usize = kinds_of(rig.session.appender())
        .into_iter()
        .skip_while(|kind| *kind != "trial.start")
        .filter(|kind| *kind == "smu.write")
        .count();
    assert_eq!(writes_after_start, 0);
    let events = rig.session.appender().journal().events().len();
    let outcome = trial(&mut rig).unwrap_err();
    assert!(matches!(outcome, SessionError::Unproven));
    assert_eq!(rig.session.appender().journal().events().len(), events);
}

#[test]
fn a_crash_beside_an_unconfirmed_containment_ends_the_trial_with_both_and_latches_the_crash() {
    let mut rig = rigged(Bench::new(2));
    {
        let mut rigs = rig.rigs.borrow_mut();
        rigs.wait_error = Some(|| Error::Containment("workload still running".into()));
        rigs.stop_error = Some(|| Error::Crashed);
    }
    let error = trial(&mut rig).unwrap_err();
    assert!(error.is_crash(), "{error:?}");
    assert_eq!(rig.session.lifecycle.fault(), Some(Fault::Crashed));
    let ends = ends(&rig.session);
    let [end] = ends.as_slice() else {
        panic!("{ends:?}");
    };
    assert_eq!(end.outcome, Verdict::Inconclusive);
    assert!(end.interrupted);
    assert_eq!(
        end.containment_error.as_deref(),
        Some("trial cleanup could not be confirmed: workload still running")
    );
}

#[test]
fn a_machine_that_crashes_mid_trial_leaves_the_trial_open_for_the_recovery() {
    let mut rig = rigged(Bench::new(2));
    {
        let mut rigs = rig.rigs.borrow_mut();
        rigs.wait_error = Some(|| Error::Crashed);
        rigs.result = Some(TrialResult {
            ran: Duration::from_secs(10),
            ..TrialResult::default()
        });
    }
    let error = trial(&mut rig).unwrap_err();
    assert!(error.is_crash());
    assert_eq!(rig.session.lifecycle.fault(), Some(Fault::Crashed));
    assert!(ends(&rig.session).is_empty());
    assert_eq!(kind_count(&rig.session, "trial.start"), 1);
}

#[test]
fn a_crash_in_the_wait_during_a_failed_report_withholds_the_journal_emergency() {
    crash_during_a_failed_report(|rigging| rigging.wait_error = Some(|| Error::Crashed));
}

#[test]
fn a_crash_in_the_stop_during_a_failed_report_withholds_the_journal_emergency() {
    crash_during_a_failed_report(|rigging| rigging.stop_error = Some(|| Error::Crashed));
}

#[test]
fn a_crash_beside_unconfirmed_containment_during_a_failed_report_is_withheld_as_a_crash() {
    crash_during_a_failed_report(|rigging| {
        rigging.wait_error = Some(|| Error::Containment("scope still populated".into()));
        rigging.stop_error = Some(|| Error::Crashed);
    });
}

#[test]
fn a_crash_in_the_teardown_kernel_read_during_a_failed_report_withholds_the_journal_emergency() {
    let mut rig = rigged(Bench::new(2));
    rig.session.validated = Some(rig.session.host.validate_smu().unwrap());
    rig.rigs.borrow_mut().result = Some(failing_with(Signal::ComputationError));
    lose_read(&rig.log, 2, || Error::Crashed);
    let mem = rig.bench.mem.clone();
    rig.rigs.borrow_mut().after_wait =
        Some(Rc::new(move || mem.inject(FsFault::before(Op::Write, 4))));
    let touched = Rc::new(Cell::new(0_usize));
    let count = touched.clone();
    rig.bench.sim.interrupt_smu(move |_: &SmuOperation| {
        count.set(count.get() + 1);
        Ok(())
    });
    let error = trial(&mut rig).unwrap_err();
    assert!(matches!(error, SessionError::Journal(_)), "{error:?}");
    assert_eq!(rig.log.borrow().reads, 3);
    assert!(ends(&rig.session).is_empty());
    let before_close = touched.get();
    let ended = rig.session.close(Err(error));
    assert!(
        matches!(
            ended.emergency.as_ref().map(|outcome| &outcome.response),
            Some(crate::Response::Withheld(crate::Withheld::Crashed))
        ),
        "{:?}",
        ended.emergency
    );
    assert_eq!(touched.get(), before_close);
}

fn crash_during_a_failed_report(crash: impl FnOnce(&mut Rigging)) {
    let mut rig = rigged(Bench::new(2));
    rig.session.validated = Some(rig.session.host.validate_smu().unwrap());
    rig.bench
        .sim
        .script_trial(
            "0001",
            Script {
                signal: Some(Signal::ComputationError),
                at_s: 10.0,
                core: core(0),
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
    let touched = Rc::new(Cell::new(0_usize));
    let count = touched.clone();
    rig.bench.sim.interrupt_smu(move |_: &SmuOperation| {
        count.set(count.get() + 1);
        Ok(())
    });
    {
        let mut rigs = rig.rigs.borrow_mut();
        let mem = rig.bench.mem.clone();
        rigs.on_start = Some(Rc::new(move || {
            mem.inject(FsFault::before(Op::Write, 2));
        }));
        crash(&mut rigs);
    }
    let error = trial(&mut rig).unwrap_err();
    assert!(matches!(error, SessionError::Journal(_)), "{error:?}");
    assert_eq!(kind_count(&rig.session, "trial.start"), 1);
    assert_eq!(kind_count(&rig.session, "trial.progress"), 0);
    assert!(ends(&rig.session).is_empty());
    let before_close = touched.get();
    let ended = rig.session.close(Err(error));
    assert!(
        matches!(
            ended.emergency.as_ref().map(|outcome| &outcome.response),
            Some(crate::Response::Withheld(crate::Withheld::Crashed))
        ),
        "{:?}",
        ended.emergency
    );
    assert_eq!(touched.get(), before_close);
}

#[test]
fn a_backend_failure_the_workload_reports_is_journaled_as_progress_citing_the_start() {
    let mut rig = rigged(Bench::new(2));
    rig.bench
        .sim
        .script_trial(
            "0001",
            Script {
                signal: Some(Signal::ComputationError),
                at_s: 10.0,
                core: core(0),
                reset: None,
                then_crash: false,
            },
        )
        .unwrap();
    trial(&mut rig).unwrap();
    let events = rig.session.appender().journal().events();
    let start = events
        .iter()
        .find(|event| matches!(event.payload, Payload::TrialStart(_)))
        .unwrap();
    let progress: Vec<_> = events
        .iter()
        .filter(|event| matches!(event.payload, Payload::TrialProgress(_)))
        .collect();
    assert!(!progress.is_empty());
    assert!(progress.iter().all(|event| event.cause == [start.seq]));
    let end = ends(&rig.session).pop().unwrap();
    assert_eq!(end.outcome, Verdict::Failure);
    assert_eq!(end.signal, Some(Signal::ComputationError));
}

#[test]
fn a_journal_that_fails_anywhere_in_a_trial_leaves_no_workload_running() {
    let mut failed = 0;
    for nth in 1..=24 {
        let mut rig = rigged(Bench::new(2));
        rig.bench.mem.inject(FsFault::before(Op::Write, nth));
        let outcome = trial(&mut rig);
        assert!(
            rig.bench.sim.violations().is_empty(),
            "write {nth}: {:?}",
            rig.bench.sim.violations()
        );
        if outcome.is_err() {
            failed += 1;
            assert!(
                rig.session.lifecycle.stopped().is_some()
                    || rig.session.lifecycle.fault().is_some(),
                "write {nth}"
            );
        }
    }
    assert!(failed >= 8, "{failed}");
}

#[test]
fn a_trial_loads_the_first_cpu_of_each_joint_core_and_as_many_of_a_lone_core_as_the_workload_has_threads()
 {
    let rig = rigged(Bench::new(2));
    let topology = rig.session.topology.clone();
    let joint = together_plan(&rig.session, &[0, 0]);
    let workload = joint.workload_at(0);
    let (cores, cpus) = rig.session.assign(&joint, workload).unwrap();
    assert_eq!(cores, [core(0), core(1)]);
    assert_eq!(cpus, [topology[0].cpus[0], topology[1].cpus[0]]);

    let lone = search_plan(&rig.session, core(1), Regime::R1);
    let workload = lone.workload_at(0);
    let (cores, cpus) = rig.session.assign(&lone, workload).unwrap();
    assert_eq!(cores, [core(1)]);
    let threads = usize::from(workload.workload().threads).max(1);
    assert_eq!(
        cpus,
        topology[1].cpus[..threads.min(topology[1].cpus.len())]
    );

    let missing = search_plan(&rig.session, core(0), Regime::R1);
    let mut missing = missing;
    missing.load = togi_tuner::Load::Single(core(9));
    assert!(matches!(
        rig.session.assign(&missing, workload),
        Err(SessionError::NoSuchTrialCore { core: found }) if found == core(9)
    ));
}

/// What a whole run did, and how often it touched the SMU once its first trial's workload had started.
struct Whole {
    ended: crate::Ended,
    smu_after_start: usize,
}

#[derive(Clone, Copy, Default)]
struct Faults {
    /// The journal write, counted from the workload's start, that fails.
    write_at_start: Option<u64>,
    /// The journal write, counted from the return of the workload's wait, that fails.
    write_after_wait: Option<u64>,
}

fn whole(faults: Faults, rigging: impl FnOnce(&mut Rigging)) -> Whole {
    let bench = Bench::new(2);
    let (input, _log, rigs, _waits, _fail_sleep) = rig_input(&bench);
    let armed = Rc::new(Cell::new(false));
    let touched = Rc::new(Cell::new(0_usize));
    {
        let (armed, touched) = (armed.clone(), touched.clone());
        bench.sim.interrupt_smu(move |_: &SmuOperation| {
            if armed.get() {
                touched.set(touched.get() + 1);
            }
            Ok(())
        });
    }
    {
        let mut rigs = rigs.borrow_mut();
        let (mem, flag) = (bench.mem.clone(), armed);
        rigs.on_start = Some(Rc::new(move || {
            flag.set(true);
            if let Some(nth) = faults.write_at_start {
                mem.inject(FsFault::before(Op::Write, nth));
            }
        }));
        if let Some(nth) = faults.write_after_wait {
            let mem = bench.mem.clone();
            rigs.after_wait = Some(Rc::new(move || mem.inject(FsFault::before(Op::Write, nth))));
        }
        rigging(&mut rigs);
    }
    let ended = crate::run(input);
    Whole {
        ended,
        smu_after_start: touched.get(),
    }
}

impl Whole {
    fn kinds(&self) -> Vec<&'static str> {
        self.ended
            .journal
            .events()
            .iter()
            .map(|event| event.kind().as_str())
            .collect()
    }

    fn withheld(&self) -> Option<crate::Withheld> {
        if let Some(crate::Response::Withheld(withheld)) = self
            .ended
            .emergency
            .as_ref()
            .map(|emergency| &emergency.response)
        {
            Some(*withheld)
        } else {
            None
        }
    }

    /// The run ended without recording anything after the failure it started from.
    fn leaves_the_machine_and_the_journal_alone(&self) {
        assert_eq!(self.smu_after_start, 0, "{:?}", self.kinds());
        let kinds = self.kinds();
        for forbidden in ["shutdown", "profile.restored", "smu.intent", "smu.write"] {
            let after = kinds.iter().skip_while(|kind| **kind != "trial.intent");
            let after = after.skip_while(|kind| **kind != "trial.start" && **kind != "trial.end");
            assert!(
                !after.clone().any(|kind| *kind == forbidden),
                "{forbidden} after the trial began: {kinds:?}"
            );
        }
    }
}

fn contained() -> Error {
    Error::Containment("workload still running".into())
}

#[test_case("start, containment", contained as fn() -> Error, false, crate::Withheld::TeardownUnconfirmed; "start containment")]
#[test_case("start, crash", || Error::Crashed, false, crate::Withheld::Crashed; "start crash")]
#[test_case("signal, containment", contained, true, crate::Withheld::TeardownUnconfirmed; "signal containment")]
#[test_case("signal, crash", || Error::Crashed, true, crate::Withheld::Crashed; "signal crash")]
fn a_failed_start_record_and_a_failed_join_leave_the_machine_alone(
    name: &str,
    stop: fn() -> Error,
    signal: bool,
    want: crate::Withheld,
) {
    let run = whole(
        Faults {
            write_at_start: Some(if signal { 2 } else { 1 }),
            ..Faults::default()
        },
        |rigging| {
            rigging.stop_error = Some(stop);
            if signal {
                rigging.schedule = Some(LoadSchedule::Random { seed: 1 });
            }
        },
    );
    let kinds = run.kinds();
    assert!(!run.ended.errors.is_empty(), "{name}");
    assert_eq!(run.ended.stop, None, "{name}");
    assert_eq!(run.withheld(), Some(want), "{name}: {kinds:?}");
    assert!(
        kinds.contains(&"trial.start") == signal,
        "{name}: {kinds:?}"
    );
    assert!(!kinds.contains(&"trial.signal"), "{name}: {kinds:?}");
    assert!(!kinds.contains(&"trial.end"), "{name}: {kinds:?}");
    run.leaves_the_machine_and_the_journal_alone();
}

#[test]
fn a_failed_start_record_of_a_joined_workload_zeroes_the_machine_as_the_emergency() {
    let run = whole(
        Faults {
            write_at_start: Some(1),
            ..Faults::default()
        },
        |_| {},
    );
    assert!(
        matches!(
            run.ended
                .emergency
                .as_ref()
                .map(|emergency| &emergency.response),
            Some(crate::Response::Zeroed(_))
        ),
        "{:?}",
        run.kinds()
    );
    assert!(run.smu_after_start > 0);
}

fn containment_dead_end(run: &Whole) -> TrialEnd {
    assert!(run.ended.errors.is_empty(), "{:?}", run.ended.errors);
    assert!(
        matches!(&run.ended.stop, Some(Stop::DeadEnd { dead_end, .. })
            if dead_end.condition == DeadEndCondition::Containment),
        "{:?}",
        run.ended.stop
    );
    assert!(run.ended.emergency.is_none());
    run.leaves_the_machine_and_the_journal_alone();
    let end = run
        .ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(end.outcome, Verdict::Inconclusive);
    assert!(end.containment_error.is_some());
    end
}

#[test]
fn a_start_that_returns_no_proof_dead_ends_on_containment_and_the_machine_is_left_alone() {
    let run = whole(Faults::default(), |rigging| {
        rigging.start_error = Some(contained);
    });
    containment_dead_end(&run);

    let run = whole(
        Faults {
            write_at_start: Some(1),
            ..Faults::default()
        },
        |rigging| rigging.start_error = Some(contained),
    );
    assert!(!run.ended.errors.is_empty());
    assert_eq!(run.withheld(), Some(crate::Withheld::TeardownUnconfirmed));
    assert!(!run.kinds().contains(&"trial.end"), "{:?}", run.kinds());
    run.leaves_the_machine_and_the_journal_alone();
}

#[test]
fn an_unconfirmed_partial_start_journals_each_setup_and_cleanup_cause_once() {
    const FAILURES: &str = "the start failed: \
        launch instance c01 in scope togi-trial-t1-c01: systemd-run missing; \
        reap instance c00: the leader did not exit; \
        backend exit and output drain not confirmed before the teardown deadline; \
        a process remains in scope togi-trial-t1-c00 at the teardown deadline";
    let run = whole(Faults::default(), |rigging| {
        rigging.start_error = Some(|| Error::Containment(FAILURES.into()));
    });
    let end = containment_dead_end(&run);
    assert_eq!(
        end.containment_error,
        Some(format!("trial cleanup could not be confirmed: {FAILURES}"))
    );
    assert_eq!(
        end.reason,
        Some(format!(
            "setup failed: trial cleanup could not be confirmed: {FAILURES}"
        ))
    );
}

#[test]
fn a_transparent_shared_cause_is_journaled_once() {
    #[derive(Debug, thiserror::Error)]
    #[error("the start failed: launch backend")]
    struct Kept(#[source] std::io::Error);

    #[derive(Debug)]
    struct Shared(Kept);

    impl std::fmt::Display for Shared {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            std::fmt::Display::fmt(&self.0, f)
        }
    }

    impl std::error::Error for Shared {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            std::error::Error::source(&self.0)
        }
    }

    let run = whole(Faults::default(), |rigging| {
        rigging.start_error = Some(|| {
            Error::Containment(Box::new(Shared(Kept(std::io::Error::other(
                "launch cause",
            )))))
        });
    });
    let end = containment_dead_end(&run);
    let text =
        "trial cleanup could not be confirmed: the start failed: launch backend: launch cause";
    assert_eq!(end.containment_error.as_deref(), Some(text));
    assert_eq!(end.reason, Some(format!("setup failed: {text}")));
}

#[test]
fn an_unconfirmed_non_containment_start_journals_its_source() {
    #[derive(Debug, thiserror::Error)]
    #[error("launch backend")]
    struct Setup(#[source] std::io::Error);

    let run = whole(Faults::default(), |rigging| {
        rigging.start_error =
            Some(|| Error::Other(Box::new(Setup(std::io::Error::other("launch cause")))));
    });
    let end = containment_dead_end(&run);
    assert_eq!(
        end.containment_error.as_deref(),
        Some("launch backend: launch cause")
    );
    assert_eq!(
        end.reason.as_deref(),
        Some("setup failed: launch backend: launch cause")
    );
}

#[test_case((|rigging: &mut Rigging| rigging.wait_error = Some(contained)) as fn(&mut Rigging); "(|rigging: &mut Rigging| rigging.wait_error = Some(contained)) as fn(&mut Rigging)")]
#[test_case(|rigging: &mut Rigging| rigging.stop_error = Some(contained); "|rigging: &mut Rigging| rigging.stop_error = Some(contained)")]
fn containment_from_wait_or_stop_journals_its_cause(fail: fn(&mut Rigging)) {
    let run = whole(Faults::default(), fail);
    let end = containment_dead_end(&run);
    assert_eq!(
        end.containment_error.as_deref(),
        Some("trial cleanup could not be confirmed: workload still running")
    );
}

#[test]
fn a_containment_error_from_the_wait_withholds_the_proof_a_successful_stop_gives() {
    let run = whole(Faults::default(), |rigging| {
        rigging.wait_error = Some(contained);
    });
    containment_dead_end(&run);

    let run = whole(
        Faults {
            write_after_wait: Some(1),
            ..Faults::default()
        },
        |rigging| rigging.wait_error = Some(contained),
    );
    assert!(!run.ended.errors.is_empty());
    assert_eq!(run.withheld(), Some(crate::Withheld::TeardownUnconfirmed));
    assert!(!run.kinds().contains(&"trial.end"), "{:?}", run.kinds());
    run.leaves_the_machine_and_the_journal_alone();

    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().wait_error = Some(contained);
    trial(&mut rig).unwrap();
    assert_eq!(rig.session.lifecycle.fault(), Some(Fault::Containment));
    assert!(matches!(
        trial(&mut rig).unwrap_err(),
        SessionError::Unproven
    ));
}

#[test]
fn a_start_that_returns_no_proof_latches_the_containment_and_holds_no_proof() {
    let mut rig = rigged(Bench::new(2));
    rig.rigs.borrow_mut().start_error = Some(contained);
    trial(&mut rig).unwrap();
    assert_eq!(rig.session.lifecycle.fault(), Some(Fault::Containment));
    let end = ends(&rig.session).pop().unwrap();
    assert!(end.containment_error.is_some());
    assert!(matches!(
        trial(&mut rig).unwrap_err(),
        SessionError::Unproven
    ));
}

#[test]
fn an_escaped_worker_ends_the_trial_inconclusive_with_its_cpus_and_dead_ends_on_containment() {
    let run = whole(Faults::default(), |rigging| {
        rigging.result = Some(TrialResult {
            ran: full_run(),
            escaped: vec![CpuId::from(42)],
            ..TrialResult::default()
        });
    });
    assert!(run.ended.errors.is_empty(), "{:?}", run.ended.errors);
    assert!(
        matches!(&run.ended.stop, Some(Stop::DeadEnd { dead_end, .. })
            if dead_end.condition == DeadEndCondition::Containment),
        "{:?}",
        run.ended.stop
    );
    let kinds = run.kinds();
    assert_eq!(
        kinds.iter().filter(|kind| **kind == "trial.start").count(),
        1
    );
    let after_end = kinds.iter().skip_while(|kind| **kind != "trial.end");
    assert!(
        !after_end
            .clone()
            .any(|kind| matches!(*kind, "smu.write" | "profile.restored")),
        "{kinds:?}"
    );
    let ends: Vec<TrialEnd> = run
        .ended
        .journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].outcome, Verdict::Inconclusive);
    assert_eq!(ends[0].containment_error, None);
    assert_eq!(ends[0].escaped, [CpuId::from(42)]);
}

fn public_fault_run(
    controller: togi_machine::ControllerContract,
    cpu: Option<u16>,
    worker: bool,
    backend: Option<Signal>,
    backend_pid: u32,
) -> crate::session::Ended {
    let bench = Bench::new(2);
    let (mut input, log, rigs, _, _) = rig_input(&bench);
    input.controller = controller;
    let mut rig = rigs.borrow_mut();
    rig.pid = Some(991);
    if backend_pid != 991 {
        rig.instances = Some(same_core_workers(backend_pid));
    }
    rig.cancel = Some(input.cancel.clone());
    rig.result = Some(TrialResult {
        ran: Duration::from_secs(90),
        failure: backend.map(|signal| TrialFailure {
            signal,
            core: core(0),
            pid: std::num::NonZeroU32::new(backend_pid),
        }),
        ..TrialResult::default()
    });
    rig.after_wait = Some(Rc::new(move || {
        let mut log = log.borrow_mut();
        let monotonic = log.clock.monotonic().unwrap();
        log.faults.push(togi_machine::ProcessFault {
            pid: if worker { 991 } else { std::process::id() },
            name: if worker { "mprime" } else { "togi-dashboard" }.into(),
            scope: None,
            kind: togi_machine::ProcessFaultKind::Segmentation,
            cpu: cpu.map(CpuId::from),
            core: cpu.map(|cpu| core(i64::from(cpu))),
            monotonic: monotonic.saturating_sub(Duration::from_nanos(1)),
            lines: vec!["kernel trap".into()],
        });
        log.faults.push(togi_machine::ProcessFault {
            pid: if worker { 991 } else { std::process::id() },
            name: if worker { "mprime" } else { "togi" }.into(),
            scope: None,
            kind: togi_machine::ProcessFaultKind::Segmentation,
            cpu: None,
            core: None,
            monotonic,
            lines: vec!["systemd-coredump summary without an execution CPU".into()],
        });
    }));
    drop(rig);
    crate::run(input)
}

#[test_case(Some(0), false, None, Verdict::Failure, Some(Signal::UnexpectedExit); "controller located")]
#[test_case(None, false, None, Verdict::Failure, Some(Signal::UnexpectedExit); "controller unlocated")]
#[test_case(Some(1), false, None, Verdict::Pass, None; "controller outside tested set")]
#[test_case(Some(0), true, Some(Signal::UnexpectedExit), Verdict::Failure, Some(Signal::UnexpectedExit); "worker located")]
#[test_case(Some(1), true, Some(Signal::UnexpectedExit), Verdict::Inconclusive, None; "outside worker exit is not charged")]
#[test_case(Some(1), true, Some(Signal::ComputationError), Verdict::Failure, Some(Signal::ComputationError); "independent computation evidence survives")]
fn managed_public_session_adjudicates_process_faults(
    cpu: Option<u16>,
    worker: bool,
    backend: Option<Signal>,
    outcome: Verdict,
    signal: Option<Signal>,
) {
    let ended = public_fault_run(
        togi_machine::ControllerContract::Managed,
        cpu,
        worker,
        backend,
        991,
    );
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let end = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(end.outcome, outcome);
    assert_eq!(end.signal, signal);
    let fault = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialProgress(progress) = &event.payload {
                progress.fault.as_ref()
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(fault.cpu, cpu.map(CpuId::from));
    assert_eq!(fault.core, cpu.map(|cpu| core(i64::from(cpu))));
    let start = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialStart(start) = &event.payload {
                Some(start)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(start.controller.as_ref().unwrap().pid, std::process::id());
    assert!(
        start
            .controller
            .as_ref()
            .unwrap()
            .tested_cpus
            .contains(&CpuId::from(0))
    );
}

#[test]
fn legacy_public_session_does_not_adopt_process_fault_decisions() {
    let ended = public_fault_run(
        togi_machine::ControllerContract::Legacy,
        Some(1),
        true,
        Some(Signal::UnexpectedExit),
        991,
    );
    let end = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(end.outcome, Verdict::Failure);
    assert_eq!(end.signal, Some(Signal::UnexpectedExit));
}

fn same_core_workers(other_pid: u32) -> Vec<Instance> {
    [991, other_pid]
        .into_iter()
        .map(|pid| Instance {
            core: core(0),
            cpus: vec![CpuId::from(0)],
            pid,
            scope: format!("worker-{pid}.scope"),
        })
        .collect()
}

#[test_case(Some(0); "located sibling fault")]
#[test_case(Some(1); "outside sibling fault")]
fn another_same_core_worker_exit_is_not_replaced_or_withdrawn(cpu: Option<u16>) {
    let ended = public_fault_run(
        togi_machine::ControllerContract::Managed,
        cpu,
        true,
        Some(Signal::UnexpectedExit),
        992,
    );
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let end = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(end.outcome, Verdict::Failure);
    assert_eq!(end.signal, Some(Signal::UnexpectedExit));
    assert_eq!(end.core, Some(core(0)));
}

#[test_case(Some(0), Verdict::Failure, Some(core(0)), None, None, 991; "located controller death")]
#[test_case(None, Verdict::Failure, None, None, None, 991; "unlocated controller death")]
#[test_case(Some(1), Verdict::Inconclusive, None, None, None, 991; "outside tested set")]
#[test_case(Some(0), Verdict::Failure, Some(core(0)), Some(421), None, 991; "known dashboard thread death")]
#[test_case(Some(0), Verdict::Failure, Some(core(0)), Some(422), None, 991; "known worker thread located")]
#[test_case(Some(1), Verdict::Inconclusive, None, Some(991), Some(Signal::UnexpectedExit), 991; "outside worker exit withdrawn on recovery")]
#[test_case(Some(1), Verdict::Inconclusive, None, Some(422), Some(Signal::UnexpectedExit), 991; "outside known worker thread exit withdrawn")]
#[test_case(Some(1), Verdict::Failure, Some(core(0)), Some(991), Some(Signal::ComputationError), 991; "independent failure retained on recovery")]
#[test_case(None, Verdict::Failure, None, Some(991), Some(Signal::UnexpectedExit), 991; "unlocated worker does not inherit assigned core")]
#[test_case(Some(1), Verdict::Failure, Some(core(0)), Some(991), Some(Signal::UnexpectedExit), 992; "another same-core worker exit retained on recovery")]
fn public_same_boot_recovery_uses_the_saved_controller_contract(
    cpu: Option<u16>,
    outcome: Verdict,
    location: Option<togi_machine::CoreId>,
    fault_pid: Option<u32>,
    backend: Option<Signal>,
    backend_pid: u32,
) {
    let bench = Bench::new(2);
    let (mut input, log, rigs, _, _) = rig_input(&bench);
    input.controller = togi_machine::ControllerContract::Managed;
    let evidence = log.clone();
    rigs.borrow_mut().wait_error = Some(|| Error::Crashed);
    rigs.borrow_mut().pid = Some(991);
    if backend_pid != 991 {
        rigs.borrow_mut().instances = Some(same_core_workers(backend_pid));
    }
    rigs.borrow_mut().reported_failure = backend.map(|signal| TrialFailure {
        signal,
        core: core(0),
        pid: std::num::NonZeroU32::new(backend_pid),
    });
    rigs.borrow_mut().after_wait = Some(Rc::new(move || {
        let mut log = evidence.borrow_mut();
        let monotonic = log.clock.monotonic().unwrap();
        log.faults.push(togi_machine::ProcessFault {
            pid: fault_pid.unwrap_or_else(std::process::id),
            name: "renamed-thread".into(),
            scope: None,
            kind: togi_machine::ProcessFaultKind::Abort,
            cpu: cpu.map(CpuId::from),
            core: cpu.map(|cpu| core(i64::from(cpu))),
            monotonic,
            lines: vec!["controller abort".into()],
        });
    }));
    let crashed = crate::run(input);
    assert!(crashed.errors.iter().any(SessionError::is_crash));
    assert!(
        !crashed
            .journal
            .events()
            .iter()
            .any(|event| matches!(event.payload, Payload::TrialEnd(_)))
    );
    drop(crashed);
    let mut input = bench.input();
    // Default scheduling is still legacy, but the interrupted trial's durable opt-in owns its evidence.
    assert_eq!(input.controller, togi_machine::ControllerContract::Legacy);
    let inner = input.machine.kernel;
    input.machine.kernel = Box::new(LogKernel { log, inner });
    let cancel = input.cancel.clone();
    input.on_append = Some(Box::new(move |event| {
        if matches!(event.payload, Payload::TrialEnd(_)) {
            cancel.cancel();
        }
    }));
    let resumed = crate::run(input);
    assert!(resumed.errors.is_empty(), "{:?}", resumed.errors);
    let (event, end) = resumed
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some((event, end))
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(end.outcome, outcome);
    assert_eq!(end.core, location);
    assert_eq!(
        end.signal,
        (outcome == Verdict::Failure).then_some(backend.unwrap_or(Signal::UnexpectedExit))
    );
    assert!(end.interrupted);
    let fault_event = resumed.journal.events().iter().find(|event| {
        matches!(&event.payload, Payload::TrialProgress(progress) if progress.fault.is_some())
    }).unwrap();
    assert!(event.cause.contains(&fault_event.seq));
}

fn trial_end(ended: &crate::session::Ended) -> TrialEnd {
    ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end.clone())
            } else {
                None
            }
        })
        .unwrap()
}

fn located_fault(pid: u32, cpu: u16, monotonic: Duration) -> togi_machine::ProcessFault {
    togi_machine::ProcessFault {
        pid,
        name: "mprime".into(),
        scope: None,
        kind: togi_machine::ProcessFaultKind::Segmentation,
        cpu: Some(CpuId::from(cpu)),
        core: Some(core(i64::from(cpu))),
        monotonic,
        lines: vec!["kernel trap".into()],
    }
}

#[test_case([992, 991], false; "live sibling fault before the withdrawing fault")]
#[test_case([991, 992], false; "live sibling fault after the withdrawing fault")]
#[test_case([992, 991], true; "recovered sibling fault before the withdrawing fault")]
#[test_case([991, 992], true; "recovered sibling fault after the withdrawing fault")]
fn a_withdrawn_worker_exit_keeps_another_workers_tested_fault(order: [u32; 2], crash: bool) {
    let bench = Bench::new(2);
    let (mut input, log, rigs, _, _) = rig_input(&bench);
    input.controller = togi_machine::ControllerContract::Managed;
    let evidence = log.clone();
    {
        let mut rig = rigs.borrow_mut();
        rig.pid = Some(991);
        rig.instances = Some(same_core_workers(992));
        let failure = TrialFailure {
            signal: Signal::UnexpectedExit,
            core: core(0),
            pid: std::num::NonZeroU32::new(991),
        };
        if crash {
            rig.wait_error = Some(|| Error::Crashed);
            rig.reported_failure = Some(failure);
        } else {
            rig.cancel = Some(input.cancel.clone());
            rig.result = Some(TrialResult {
                ran: Duration::from_secs(90),
                failure: Some(failure),
                ..TrialResult::default()
            });
        }
        // Worker 991, whose exit the backend reported, executed outside the tested CPUs; its sibling 992 inside.
        rig.after_wait = Some(Rc::new(move || {
            let mut log = evidence.borrow_mut();
            let monotonic = log.clock.monotonic().unwrap();
            let cpu = |pid| u16::from(pid == 991);
            let [first, second] = order;
            let earlier = monotonic.saturating_sub(Duration::from_nanos(1));
            log.faults.push(located_fault(first, cpu(first), earlier));
            log.faults
                .push(located_fault(second, cpu(second), monotonic));
        }));
    }
    let ended = if crash {
        let crashed = crate::run(input);
        assert!(crashed.errors.iter().any(SessionError::is_crash));
        drop(crashed);
        let mut input = bench.input();
        let inner = input.machine.kernel;
        input.machine.kernel = Box::new(LogKernel { log, inner });
        let cancel = input.cancel.clone();
        input.on_append = Some(Box::new(move |event| {
            if matches!(event.payload, Payload::TrialEnd(_)) {
                cancel.cancel();
            }
        }));
        crate::run(input)
    } else {
        crate::run(input)
    };
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let end = trial_end(&ended);
    assert_eq!(end.outcome, Verdict::Failure);
    assert_eq!(end.signal, Some(Signal::UnexpectedExit));
    assert_eq!(end.core, Some(core(0)));
}

#[derive(Clone, Copy, Debug)]
enum Edge {
    BeforeStart,
    Start,
    End,
    AfterEnd,
}

#[test_case(Edge::BeforeStart, Verdict::Pass; "just before the window start is ignored")]
#[test_case(Edge::Start, Verdict::Failure; "at the window start is the trial's")]
#[test_case(Edge::End, Verdict::Failure; "at the window end is the trial's")]
#[test_case(Edge::AfterEnd, Verdict::Pass; "just after the window end is ignored")]
fn public_session_takes_process_faults_only_inside_the_trial_window(edge: Edge, outcome: Verdict) {
    let bench = Bench::new(2);
    let (mut input, log, rigs, _, _) = rig_input(&bench);
    input.controller = togi_machine::ControllerContract::Managed;
    let push = move |anchor: &[Edge]| {
        let log = log.clone();
        let anchor = anchor.to_vec();
        Rc::new(move || {
            if !anchor
                .iter()
                .any(|at| std::mem::discriminant(at) == std::mem::discriminant(&edge))
            {
                return;
            }
            let mut log = log.borrow_mut();
            let now = log.clock.monotonic().unwrap();
            let monotonic = match edge {
                Edge::BeforeStart => now.checked_sub(Duration::from_nanos(1)).unwrap(),
                Edge::Start | Edge::End => now,
                Edge::AfterEnd => now + Duration::from_nanos(1),
            };
            // The controller faulting on a tested CPU fails the trial when its fault counts.
            log.faults
                .push(located_fault(std::process::id(), 0, monotonic));
        }) as Rc<dyn Fn()>
    };
    {
        let mut rig = rigs.borrow_mut();
        rig.cancel = Some(input.cancel.clone());
        rig.on_start = Some(push(&[Edge::BeforeStart, Edge::Start]));
        rig.after_wait = Some(push(&[Edge::End, Edge::AfterEnd]));
    }
    // The boot's clock starts at zero; a trial window needs room before it.
    let cancel = input.cancel.clone();
    input
        .machine
        .clock
        .sleep(&cancel, Duration::from_secs(1))
        .unwrap();
    let ended = crate::run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let fault = ended.journal.events().iter().find_map(|event| {
        if let Payload::TrialProgress(progress) = &event.payload {
            progress.fault.as_ref()
        } else {
            None
        }
    });
    assert_eq!(fault.is_some(), outcome == Verdict::Failure, "{fault:?}");
    assert_eq!(trial_end(&ended).outcome, outcome);
}

#[test_case(false; "complete kernel read")]
#[test_case(true; "partial kernel read")]
fn public_multicore_session_uses_execution_core_instead_of_assigned_worker_core(partial: bool) {
    let mut bench = Bench::new(2);
    bench.config = Config::parse(
        r#"{
        "candidate_solo_limits":{"0":0,"1":0},
        "checking":{"cycle":["R7"]},
        "evidence":{"rate":0.95,"miss":0.2}
    }"#,
    )
    .unwrap();
    let (mut input, log, rigs, _, _) = rig_input(&bench);
    input.controller = togi_machine::ControllerContract::Managed;
    let specs = Rc::downgrade(&rigs);
    let cancel = input.cancel.clone();
    rigs.borrow_mut().pid = Some(991);
    rigs.borrow_mut().result_for_spec = Some(|spec| TrialResult {
        ran: spec.duration,
        failure: (spec.cores.len() > 1).then_some(TrialFailure {
            signal: Signal::UnexpectedExit,
            core: core(0),
            pid: std::num::NonZeroU32::new(991),
        }),
        ..TrialResult::default()
    });
    rigs.borrow_mut().after_wait = Some(Rc::new(move || {
        if specs
            .upgrade()
            .unwrap()
            .borrow()
            .specs
            .last()
            .unwrap()
            .cores
            .len()
            < 2
        {
            return;
        }
        let mut evidence = log.borrow_mut();
        let monotonic = evidence.clock.monotonic().unwrap();
        evidence.faults.push(togi_machine::ProcessFault {
            pid: 991,
            name: "mprime".into(),
            scope: None,
            kind: togi_machine::ProcessFaultKind::Segmentation,
            cpu: Some(CpuId::from(1)),
            core: Some(core(1)),
            monotonic,
            lines: vec!["mprime[991]: segfault on CPU 1".into()],
        });
        drop(evidence);
        if partial {
            lose_read(&log, 1, failure);
        }
        cancel.cancel();
    }));
    let ended = crate::run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let end = ended
        .journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
        })
        .next_back()
        .unwrap();
    assert_eq!(end.outcome, Verdict::Failure);
    assert_eq!(end.signal, Some(Signal::UnexpectedExit));
    assert_eq!(end.core, Some(core(1)));
    assert_eq!(end.kernel_error.is_some(), partial);
}

#[test_case(Some("/system.slice/togi-watch.service"), Verdict::Failure; "dashboard service scope")]
#[test_case(Some("/system.slice/other-togi-watch.service"), Verdict::Pass; "scope component must match exactly")]
#[test_case(None, Verdict::Pass; "comm alone does not identify a process")]
fn public_session_matches_dashboard_scope_without_guessing_from_comm(
    scope: Option<&'static str>,
    outcome: Verdict,
) {
    let bench = Bench::new(2);
    let (mut input, log, rigs, _, _) = rig_input(&bench);
    input.controller = togi_machine::ControllerContract::Managed;
    rigs.borrow_mut().cancel = Some(input.cancel.clone());
    rigs.borrow_mut().after_wait = Some(Rc::new(move || {
        let mut log = log.borrow_mut();
        let monotonic = log.clock.monotonic().unwrap();
        log.faults.push(togi_machine::ProcessFault {
            pid: if std::process::id() == 318_991 {
                318_992
            } else {
                318_991
            },
            name: "togi".into(),
            scope: scope.map(str::to_owned),
            kind: togi_machine::ProcessFaultKind::Abort,
            cpu: None,
            core: None,
            monotonic,
            lines: vec!["Process togi dumped core (SIGABRT)".into()],
        });
    }));
    let ended = crate::run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let end = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(end.outcome, outcome);
    assert_eq!(end.core, None);
}
