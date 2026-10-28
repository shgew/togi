#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

use std::io;
use test_case::test_case;

use jiff::tz::TimeZone;
use togi_journal::fs::{Fault, Op};
use togi_journal::{Build, FixStamp, Journal, MemFs, Payload, ResetTarget, SCHEMA, StateDir};
use togi_machine::{CancellationToken, CoreId};
use togi_render::Renderer;
use togi_session::{CommandError, ConfigSource, PublicationPolicy, RunInput, reset_all, run};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};

fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: 1,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: 1,
    }
}

fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

#[expect(
    clippy::disallowed_methods,
    reason = "the tests keep the journal in memory"
)]
fn dir(mem: &MemFs) -> StateDir {
    StateDir::simulated(mem.clone()).unwrap()
}

fn simulator() -> Simulator {
    simulator_of(&MachineDefinition {
        cores: 2,
        ..MachineDefinition::default()
    })
}

fn simulator_of(definition: &MachineDefinition) -> Simulator {
    Simulator::new(
        Model::new(definition, 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    )
    .unwrap()
}

fn input(sim: &Simulator, mem: &MemFs) -> RunInput<'static> {
    let mut machine = sim.seams();
    let boot = machine.host.boot_id().unwrap();
    let journal = dir(mem).open_at(boot.clone(), sim.now()).unwrap();
    RunInput {
        config: togi_config::Config::default(),
        source: ConfigSource::default(),
        build: build(),
        boot,
        journal,
        machine,
        publication: PublicationPolicy::OnFinish,
        bootloader: None,
        defects: &[],
        resume: None,
        import: None,
        log: None,
        on_append: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        report: Box::new(io::sink()),
    }
}

fn started(sim: &Simulator, mem: &MemFs) -> Journal {
    let input = input(sim, mem);
    input.cancel.cancel();
    let ended = run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    ended.journal
}

fn kinds(journal: &Journal) -> Vec<&'static str> {
    journal
        .events()
        .iter()
        .map(|event| event.kind().as_str())
        .collect()
}

#[test]
fn reset_all_archives_the_session_and_the_directory_starts_empty() {
    let (sim, mem) = (simulator(), MemFs::new());
    let journal = started(&sim, &mem);
    let (session, dir) = reset_all(&build(), journal, &mut *sim.seams().clock).unwrap();
    assert_eq!(dir.archives().unwrap(), [session]);
    assert_eq!(dir.reset_boundary().unwrap(), Some(session));
    drop(dir);
    let boot = sim.seams().host.boot_id().unwrap();
    let reopened = self::dir(&mem).open_at(boot, sim.now()).unwrap();
    assert!(reopened.events().is_empty());
}

#[test]
fn reset_all_archives_a_session_of_another_schema_unread() {
    let (sim, mem) = (simulator(), MemFs::new());
    let journal = started(&sim, &mem);
    let mut other = build();
    other.schema += 1;
    let (session, dir) = reset_all(&other, journal, &mut *sim.seams().clock).unwrap();
    assert_eq!(dir.archives().unwrap(), [session]);
}

#[test]
fn reset_all_that_cannot_archive_fails_and_releases_the_lock() {
    let (sim, mem) = (simulator(), MemFs::new());
    let journal = started(&sim, &mem);
    mem.inject(Fault::before(Op::Rename, 1));
    let error = reset_all(&build(), journal, &mut *sim.seams().clock).unwrap_err();
    assert!(matches!(error, CommandError::Archive(_)), "{error:?}");
    drop(dir(&mem));
}

#[test_case(Op::Write, false; "Op::Write, false")]
#[test_case(Op::Sync, true; "Op::Sync, true")]
fn a_failed_reset_all_append_returns_an_append_error_releases_the_lock_and_archives_nothing(
    fault: Op,
    survives: bool,
) {
    let (sim, mem) = (simulator(), MemFs::new());
    let journal = started(&sim, &mem);
    let before = kinds(&journal);
    mem.inject(Fault::before(fault, 1).named("events.jsonl"));
    let error = reset_all(&build(), journal, &mut *sim.seams().clock).unwrap_err();
    assert!(
        matches!(error, CommandError::Append(_)),
        "{fault:?}: {error:?}"
    );
    mem.clear_faults();
    let boot = sim.seams().host.boot_id().unwrap();
    let state = dir(&mem);
    assert!(state.archives().unwrap().is_empty(), "{fault:?}");
    let reopened = state.open_at(boot, sim.now()).unwrap();
    let after = kinds(&reopened);
    assert_eq!(after[..before.len()], before[..], "{fault:?}");
    if survives {
        assert_eq!(after[before.len()..], ["command.reset"], "{fault:?}");
        let event = reopened.events().last().unwrap();
        assert!(matches!(
            &event.payload,
            Payload::CommandReset(reset) if reset.target == ResetTarget::All
        ));
    } else {
        assert_eq!(after, before, "{fault:?}");
    }
}

#[test]
fn reset_all_of_an_empty_journal_is_no_session_and_releases_the_lock() {
    let (sim, mem) = (simulator(), MemFs::new());
    let boot = sim.seams().host.boot_id().unwrap();
    let journal = dir(&mem).open_at(boot.clone(), sim.now()).unwrap();
    assert!(journal.events().is_empty());
    let error = reset_all(&build(), journal, &mut *sim.seams().clock).unwrap_err();
    assert!(matches!(error, CommandError::NoSession), "{error:?}");
    let state = dir(&mem);
    assert!(state.archives().unwrap().is_empty());
    assert!(state.open_at(boot, sim.now()).unwrap().events().is_empty());
}

#[derive(Clone, Copy)]
enum ReportedTime {
    Elapsed(std::time::Duration),
    EscapedOverflow,
    CrashedSample(i64),
}

struct DurationTrials {
    inner: Box<dyn togi_machine::Trials>,
    time: ReportedTime,
    cancel: CancellationToken,
    starts: std::rc::Rc<std::cell::Cell<usize>>,
}

struct DurationRunning {
    inner: Box<dyn togi_machine::Running>,
    time: ReportedTime,
    cancel: CancellationToken,
}

impl togi_machine::Running for DurationRunning {
    fn started(&self) -> togi_machine::Started {
        self.inner.started()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn togi_machine::Reporter,
    ) -> Result<togi_machine::TrialResult, togi_machine::Partial<togi_machine::TrialResult>> {
        let mut result = self.inner.wait(cancel, report)?;
        match self.time {
            ReportedTime::Elapsed(ran) => result.ran = ran,
            ReportedTime::EscapedOverflow => {
                result.ran = std::time::Duration::from_secs(u64::from(u32::MAX) + 1);
                result.escaped.push(togi_machine::CpuId::from(63));
            }
            ReportedTime::CrashedSample(_) => {
                result.failure = Some(togi_machine::TrialFailure {
                    signal: togi_machine::Signal::ComputationError,
                    core: core(0),
                    pid: None,
                });
                return Err(togi_machine::Partial {
                    value: result,
                    error: togi_machine::Error::Crashed,
                });
            }
        }
        self.cancel.cancel();
        Ok(result)
    }

    fn stop(self: Box<Self>) -> Result<togi_machine::Stopped, togi_machine::Error> {
        self.inner.stop()
    }
}

impl togi_machine::Trials for DurationTrials {
    fn controller_pid(&self) -> u32 {
        togi_sim::CONTROLLER_PID
    }

    fn content(
        &mut self,
        workload: togi_machine::WorkloadId,
    ) -> Result<togi_machine::ContentId, togi_machine::Error> {
        self.inner.content(workload)
    }

    fn sweep(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<togi_machine::Swept, togi_machine::Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: togi_machine::TrialSpec,
        ready: togi_machine::Stopped,
    ) -> Result<Box<dyn togi_machine::Running>, togi_machine::Partial<Option<togi_machine::Stopped>>>
    {
        self.starts.set(self.starts.get() + 1);
        Ok(Box::new(DurationRunning {
            inner: self.inner.start(cancel, spec, ready)?,
            time: self.time,
            cancel: self.cancel.clone(),
        }))
    }

    fn ended(&mut self, id: &str) -> Result<(), togi_machine::Error> {
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
        if let ReportedTime::CrashedSample(elapsed_ms) = self.time {
            Box::new(std::iter::once(Ok(togi_machine::TrialConditions {
                elapsed_ms,
                ..togi_machine::TrialConditions::default()
            })))
        } else {
            self.inner.samples(id)
        }
    }
}

#[test_case::test_case(u64::from(u32::MAX) - 1, false; "below")]
#[test_case::test_case(u64::from(u32::MAX), false; "maximum")]
#[test_case::test_case(u64::from(u32::MAX) + 1, true; "overflow")]
fn trial_elapsed_seconds_are_checked(seconds: u64, refused: bool) {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        time: ReportedTime::Elapsed(std::time::Duration::from_secs(seconds)),
        cancel: input.cancel.clone(),
        starts: std::rc::Rc::default(),
    });
    let ended = run(input);
    assert_eq!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, togi_session::SessionError::TrialDuration(_))),
        refused,
        "{:?}",
        ended.errors
    );
    let duration = ended.journal.events().iter().find_map(|event| {
        if let Payload::TrialEnd(end) = &event.payload {
            Some(end.duration_s)
        } else {
            None
        }
    });
    assert_eq!(duration, u32::try_from(seconds).ok());
}

struct MonotonicClock {
    inner: Box<dyn togi_machine::Clock>,
    mono: std::time::Duration,
}

impl togi_machine::Clock for MonotonicClock {
    fn now(&self) -> jiff::Timestamp {
        self.inner.now()
    }

    fn monotonic(&self) -> Result<std::time::Duration, togi_machine::Error> {
        Ok(self.mono)
    }

    fn sleep(
        &mut self,
        cancel: &CancellationToken,
        duration: std::time::Duration,
    ) -> Result<(), togi_machine::Error> {
        self.inner.sleep(cancel, duration)
    }
}

#[test_case::test_case(-1, false; "below")]
#[test_case::test_case(0, false; "maximum")]
#[test_case::test_case(1, true; "overflow")]
fn trial_monotonic_nanoseconds_are_checked(delta: i8, refused: bool) {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    let max = std::time::Duration::from_nanos(u64::MAX);
    let nano = std::time::Duration::from_nanos(1);
    let mono = match delta {
        -1 => max.checked_sub(nano).unwrap(),
        0 => max,
        1 => max + nano,
        _ => unreachable!(),
    };
    input.machine.clock = Box::new(MonotonicClock {
        inner: input.machine.clock,
        mono,
    });
    let starts = std::rc::Rc::new(std::cell::Cell::new(0));
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        time: ReportedTime::Elapsed(std::time::Duration::from_secs(90)),
        cancel: input.cancel.clone(),
        starts: std::rc::Rc::clone(&starts),
    });
    let ended = run(input);
    assert_eq!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, togi_session::SessionError::Monotonic(_))),
        refused,
        "{:?}",
        ended.errors
    );
    let start = ended.journal.events().iter().find_map(|event| {
        if let Payload::TrialStart(start) = &event.payload {
            start.window_start_ns
        } else {
            None
        }
    });
    assert_eq!(start, u64::try_from(mono.as_nanos()).ok());
    assert_eq!(starts.get(), usize::from(!refused));
}

struct MonotonicKernel {
    inner: Box<dyn togi_machine::Kernel>,
    mono: std::time::Duration,
    visible: std::rc::Rc<std::cell::Cell<bool>>,
}

impl MonotonicKernel {
    fn mce(&self) -> togi_machine::Mce {
        togi_machine::Mce {
            cpu: togi_machine::CpuId::from(0),
            core: Some(core(0)),
            bank: 0,
            bank_type: togi_machine::BankType::Unknown,
            corrected: true,
            time: jiff::Timestamp::UNIX_EPOCH,
            monotonic: self.mono,
            lines: vec!["machine check".into()],
        }
    }
}

impl togi_machine::Kernel for MonotonicKernel {
    fn mces(
        &mut self,
        _: &togi_machine::BootId,
        _: std::time::Duration,
    ) -> Result<Vec<togi_machine::Mce>, togi_machine::Partial<Vec<togi_machine::Mce>>> {
        Ok(if self.visible.get() {
            vec![self.mce()]
        } else {
            Vec::new()
        })
    }

    fn read_mces(
        &mut self,
        boot: &togi_machine::BootId,
        after: Option<&togi_machine::KernelCursor>,
    ) -> Result<togi_machine::KernelRead, togi_machine::Partial<togi_machine::KernelRead>> {
        let mut read = self.inner.read_mces(boot, after)?;
        if self.visible.get() {
            read.mces.push(self.mce());
        }
        Ok(read)
    }

    fn reset_reason(
        &mut self,
        boot: &togi_machine::BootId,
    ) -> Result<togi_machine::ResetReason, togi_machine::Error> {
        self.inner.reset_reason(boot)
    }

    fn reset_reason_after(
        &mut self,
        boot: &togi_machine::BootId,
    ) -> Result<togi_machine::ResetReason, togi_machine::Error> {
        self.inner.reset_reason_after(boot)
    }

    fn saved_pstore(
        &mut self,
        boot: &togi_machine::BootId,
    ) -> Result<Option<togi_machine::PstoreRecord>, togi_machine::Error> {
        self.inner.saved_pstore(boot)
    }
}

#[test_case::test_case(-1, false; "below")]
#[test_case::test_case(0, false; "maximum")]
#[test_case::test_case(1, true; "overflow")]
fn kernel_monotonic_nanoseconds_are_checked(delta: i8, refused: bool) {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    let max = std::time::Duration::from_nanos(u64::MAX);
    let nano = std::time::Duration::from_nanos(1);
    let mono = match delta {
        -1 => max.checked_sub(nano).unwrap(),
        0 => max,
        1 => max + nano,
        _ => unreachable!(),
    };
    input.machine.kernel = Box::new(MonotonicKernel {
        inner: input.machine.kernel,
        mono,
        visible: std::rc::Rc::new(std::cell::Cell::new(true)),
    });
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        time: ReportedTime::Elapsed(std::time::Duration::from_secs(90)),
        cancel: input.cancel.clone(),
        starts: std::rc::Rc::default(),
    });
    let ended = run(input);
    assert_eq!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, togi_session::SessionError::Monotonic(_))),
        refused,
        "{:?}",
        ended.errors
    );
    let recorded = ended.journal.events().iter().find_map(|event| {
        if let Payload::Mce(mce) = &event.payload {
            mce.monotonic_ns
        } else {
            None
        }
    });
    assert_eq!(recorded, u64::try_from(mono.as_nanos()).ok());
}

struct SetupFailure(Box<dyn togi_machine::Trials>);

impl togi_machine::Trials for SetupFailure {
    fn controller_pid(&self) -> u32 {
        togi_sim::CONTROLLER_PID
    }

    fn content(
        &mut self,
        workload: togi_machine::WorkloadId,
    ) -> Result<togi_machine::ContentId, togi_machine::Error> {
        self.0.content(workload)
    }

    fn sweep(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<togi_machine::Swept, togi_machine::Error> {
        self.0.sweep(cancel)
    }

    fn start(
        &mut self,
        _: &CancellationToken,
        _: togi_machine::TrialSpec,
        ready: togi_machine::Stopped,
    ) -> Result<Box<dyn togi_machine::Running>, togi_machine::Partial<Option<togi_machine::Stopped>>>
    {
        Err(togi_machine::Partial {
            value: Some(ready),
            error: togi_machine::Error::Other(Box::new(io::Error::other("setup unavailable"))),
        })
    }

    fn ended(&mut self, id: &str) -> Result<(), togi_machine::Error> {
        self.0.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
        self.0.samples(id)
    }
}

#[test]
fn retry_waits_preserve_the_bounded_seconds_schedule() {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    input.machine.trials = Box::new(SetupFailure(input.machine.trials));
    let ended = run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let waits: Vec<_> = ended
        .journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::BackendRetry(retry) = &event.payload {
                Some((retry.attempt, retry.wait_s))
            } else {
                None
            }
        })
        .collect();
    // Each backend exhausts on the same schedule; the night plans around an exhausted one.
    assert!(!waits.is_empty());
    assert!(
        waits
            .chunks(3)
            .all(|schedule| schedule == [(1, 60), (2, 300), (3, 1800)]),
        "{waits:?}"
    );
}

#[test]
fn a_trial_after_a_retry_wait_starts_its_task_before_the_wait() {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    input.machine.trials = Box::new(SetupFailure(input.machine.trials));
    let ended = run(input);
    let events = ended.journal.events();
    let mut checked = 0;
    for (index, event) in events.iter().enumerate() {
        let Payload::BackendRetry(retry) = &event.payload else {
            continue;
        };
        let Some((intent, at)) = events[index..].iter().find_map(|later| {
            if let Payload::TrialIntent(intent) = &later.payload {
                Some((intent, later.time))
            } else {
                None
            }
        }) else {
            continue;
        };
        let start = intent.task_start.expect("an intent names its task start");
        assert!(
            start <= event.time,
            "{start} after the retry at {}",
            event.time
        );
        let span = at.duration_since(start).as_secs();
        assert!(
            span >= i64::from(retry.wait_s),
            "{span}s of task before {}s of wait",
            retry.wait_s
        );
        checked += 1;
    }
    assert!(checked > 0);
}

/// Trials whose `failing` backend cannot set up, and which ask the run to stop at a start `stop` picks: given the
/// backend about to start and the failed setups so far.
struct BackendGate {
    inner: Box<dyn togi_machine::Trials>,
    failing: Option<togi_machine::Backend>,
    cancel: CancellationToken,
    failed: usize,
    starts: usize,
    stop: Box<dyn Fn(togi_machine::Backend, usize, usize) -> bool>,
}

impl togi_machine::Trials for BackendGate {
    fn controller_pid(&self) -> u32 {
        togi_sim::CONTROLLER_PID
    }

    fn content(
        &mut self,
        workload: togi_machine::WorkloadId,
    ) -> Result<togi_machine::ContentId, togi_machine::Error> {
        self.inner.content(workload)
    }

    fn sweep(
        &mut self,
        cancel: &CancellationToken,
    ) -> Result<togi_machine::Swept, togi_machine::Error> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: togi_machine::TrialSpec,
        ready: togi_machine::Stopped,
    ) -> Result<Box<dyn togi_machine::Running>, togi_machine::Partial<Option<togi_machine::Stopped>>>
    {
        let backend = spec.workload.backend();
        self.starts += 1;
        if (self.stop)(backend, self.failed, self.starts) {
            self.cancel.cancel();
        }
        if self.failing == Some(backend) {
            self.failed += 1;
            return Err(togi_machine::Partial {
                value: Some(ready),
                error: togi_machine::Error::Other(Box::new(io::Error::other("setup unavailable"))),
            });
        }
        self.inner.start(cancel, spec, ready)
    }

    fn ended(&mut self, id: &str) -> Result<(), togi_machine::Error> {
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

/// The trial intents of `events` that run `backend`.
fn intents_of(events: &[togi_journal::Event], backend: togi_machine::Backend) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(&event.payload, Payload::TrialIntent(intent) if intent.workload.backend() == backend)
        })
        .count()
}

#[test_case(true; "after a completed stop")]
#[test_case(false; "after a crash")]
fn a_backend_a_closed_night_excluded_is_tried_again_by_a_same_boot_restart(stopped: bool) {
    use togi_machine::Backend;
    // A backend is excluded once it stays inconclusive through the threshold and all three retries.
    let exhausted = usize::try_from(
        togi_config::Config::default()
            .dead_ends()
            .inconclusive_in_a_row()
            .get(),
    )
    .unwrap()
        + 3;
    let sim = simulator();
    let mem = MemFs::new();
    let mut first = input(&sim, &mem);
    first.machine.trials = Box::new(BackendGate {
        inner: first.machine.trials,
        failing: Some(Backend::Mprime),
        cancel: first.cancel.clone(),
        failed: 0,
        starts: 0,
        stop: Box::new(move |backend, failed, starts| {
            backend == Backend::Ycruncher && failed >= exhausted || starts > 200
        }),
    });
    let ended = run(first);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let events = ended.journal.events();
    assert!(intents_of(events, Backend::Mprime) >= exhausted);
    let excluded = events.iter().any(|event| {
        matches!(&event.payload, Payload::NightPlan(plan)
            if plan.unavailable.iter().any(|unavailable| unavailable.backend == Backend::Mprime))
    });
    assert!(excluded, "{:?}", kinds(&ended.journal));
    let journal = ended.journal;
    let shutdown = journal
        .events()
        .iter()
        .rposition(|event| matches!(event.payload, Payload::Shutdown(_)))
        .unwrap();
    assert_eq!(shutdown + 1, journal.events().len());
    drop(journal.close().unwrap());
    if !stopped {
        // A run that never recorded its shutdown left no closing witness: the night and its exclusion go on.
        let bytes = mem.contents("/state/events.jsonl").unwrap();
        let last = bytes[..bytes.len() - 1]
            .iter()
            .rposition(|byte| *byte == b'\n')
            .unwrap();
        mem.put("/state/events.jsonl", bytes[..=last].to_vec());
    }
    let mut second = input(&sim, &mem);
    let before = second.journal.events().len();
    second.machine.trials = Box::new(BackendGate {
        inner: second.machine.trials,
        failing: None,
        cancel: second.cancel.clone(),
        failed: 0,
        starts: 0,
        stop: Box::new(|backend, _, starts| backend == Backend::Mprime || starts > 20),
    });
    let ended = run(second);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let after = &ended.journal.events()[before..];
    assert_eq!(intents_of(after, Backend::Mprime) > 0, stopped);
}

#[test]
fn a_task_a_crash_interrupts_in_its_retry_wait_keeps_its_start_across_the_restart() {
    use togi_machine::Backend;
    let sim = simulator();
    let mem = MemFs::new();
    let mut first = input(&sim, &mem);
    first.machine.trials = Box::new(BackendGate {
        inner: first.machine.trials,
        failing: Some(Backend::Mprime),
        cancel: first.cancel.clone(),
        failed: 0,
        starts: 0,
        stop: Box::new(|_, _, starts| starts > 200),
    });
    let cancel = first.cancel.clone();
    first.on_append = Some(Box::new(move |event| {
        if matches!(event.payload, Payload::BackendRetry(_)) {
            cancel.cancel();
        }
    }));
    let ended = run(first);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let journal = ended.journal;
    let retry = journal
        .events()
        .iter()
        .position(|event| matches!(event.payload, Payload::BackendRetry(_)))
        .unwrap();
    let retried_at = journal.events()[retry].time;
    drop(journal.close().unwrap());
    // The crash: nothing after the retry record reached the journal, and the machine was down for an hour.
    let bytes = mem.contents("/state/events.jsonl").unwrap();
    let kept = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .take(retry + 1)
        .map(<[u8]>::len)
        .sum();
    mem.put("/state/events.jsonl", bytes[..kept].to_vec());
    sim.seams()
        .clock
        .sleep(
            &CancellationToken::new(),
            std::time::Duration::from_secs(3600),
        )
        .unwrap();
    let mut second = input(&sim, &mem);
    let before = second.journal.events().len();
    assert_eq!(before, retry + 1);
    second.machine.trials = Box::new(BackendGate {
        inner: second.machine.trials,
        failing: None,
        cancel: second.cancel.clone(),
        failed: 0,
        starts: 0,
        stop: Box::new(|_, _, _| true),
    });
    let ended = run(second);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let resumed = ended.journal.events()[before..]
        .iter()
        .find_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload {
                Some(intent)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(resumed.workload.backend(), Backend::Mprime);
    // The interrupted wait and the downtime stay charged to the task that waited.
    let task_start = resumed.task_start.unwrap();
    assert!(
        task_start <= retried_at,
        "{task_start} is after the retry the crash interrupted at {retried_at}"
    );
}

#[test_case(&[togi_machine::Backend::Mprime]; "one backend")]
#[test_case(&[togi_machine::Backend::Mprime, togi_machine::Backend::Ycruncher]; "both backends")]
fn a_backend_missing_at_preflight_is_excluded_from_the_night_without_a_launch_or_a_wait(
    missing: &[togi_machine::Backend],
) {
    use togi_machine::Backend;
    let sim = simulator();
    for backend in missing {
        sim.miss_backend(*backend);
    }
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    input.machine.trials = Box::new(BackendGate {
        inner: input.machine.trials,
        failing: None,
        cancel: input.cancel.clone(),
        failed: 0,
        starts: 0,
        stop: Box::new(|_, _, starts| starts > 20),
    });
    let ended = run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let events = ended.journal.events();
    let checks: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::PreflightCheck(check) = &event.payload
                && check.check.starts_with("backends.")
            {
                Some((check.check.as_str(), check.ok, event.seq))
            } else {
                None
            }
        })
        .collect();
    let expected: Vec<_> = [Backend::Mprime, Backend::Ycruncher]
        .into_iter()
        .map(|backend| (format!("backends.{backend}"), !missing.contains(&backend)))
        .collect();
    assert_eq!(
        checks
            .iter()
            .map(|(check, ok, _)| ((*check).to_owned(), *ok))
            .collect::<Vec<_>>(),
        expected
    );
    for backend in missing {
        assert_eq!(intents_of(events, *backend), 0, "{backend}");
        let check = checks
            .iter()
            .find(|(check, ..)| *check == format!("backends.{backend}"))
            .unwrap()
            .2;
        let cited = events.iter().any(|event| {
            matches!(&event.payload, Payload::NightPlan(plan) if plan.unavailable.iter().any(
                |unavailable| unavailable.backend == *backend && unavailable.causes == [check]
            ))
        });
        assert!(cited, "{backend}: {:?}", kinds(&ended.journal));
    }
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.payload, Payload::BackendRetry(_))),
        "{:?}",
        kinds(&ended.journal)
    );
    let dead_ends: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Payload::DeadEnd(dead_end) = &event.payload {
                Some(dead_end.condition)
            } else {
                None
            }
        })
        .collect();
    // Only the scheduler's `Unavailable` ends the night, never a failed preflight.
    assert_eq!(dead_ends, [togi_journal::DeadEndCondition::NoEvidence]);
    if missing.len() == 1 {
        // The other backend runs its cells, but the stock controls that need the missing one never pass, so no
        // offset is ever mapped.
        assert!(intents_of(events, Backend::Ycruncher) > 0);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.payload, Payload::CcdEdge(_))),
            "{:?}",
            kinds(&ended.journal)
        );
    }
}

#[test]
fn a_stop_after_a_conclusive_stock_failure_keeps_the_failure_at_zero_dead_end() {
    let sim = simulator_of(&MachineDefinition {
        cores: 2,
        script: vec![togi_sim::definition::ScriptDef {
            trial: "0001".into(),
            signal: Some(togi_machine::Signal::ComputationError),
            ..togi_sim::definition::ScriptDef::default()
        }],
        ..MachineDefinition::default()
    });
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    let cancel = input.cancel.clone();
    input.on_append = Some(Box::new(move |event| {
        if matches!(&event.payload, Payload::TrialEnd(_)) {
            cancel.cancel();
        }
    }));
    let ended = run(input);
    assert!(ended.errors.is_empty(), "{:?}", ended.errors);
    let dead_ends: Vec<_> = ended
        .journal
        .events()
        .iter()
        .filter_map(|event| {
            if let Payload::DeadEnd(dead_end) = &event.payload {
                Some(dead_end.condition)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        dead_ends,
        [togi_journal::DeadEndCondition::FailureAtZero],
        "{:?}",
        kinds(&ended.journal)
    );
}

#[test]
fn an_overflowing_duration_keeps_the_containment_dead_end_durable() {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        time: ReportedTime::EscapedOverflow,
        cancel: input.cancel.clone(),
        starts: std::rc::Rc::default(),
    });
    let ended = run(input);
    assert!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, togi_session::SessionError::TrialDuration(_)))
    );
    assert!(ended.journal.events().iter().any(|event| matches!(
        &event.payload,
        Payload::DeadEnd(dead) if dead.condition == togi_journal::DeadEndCondition::Containment
            && dead.detail.contains("backend thread on cpu 63 outside allowed cpus")
    )));
    assert!(
        !ended
            .journal
            .events()
            .iter()
            .any(|event| matches!(&event.payload, Payload::TrialEnd(_)))
    );
}

#[test_case::test_case((i64::from(u32::MAX) - 1) * 1000, false; "below")]
#[test_case::test_case(i64::from(u32::MAX) * 1000, false; "maximum")]
#[test_case::test_case((i64::from(u32::MAX) + 1) * 1000, true; "overflow")]
#[test_case::test_case(-1, true; "negative")]
fn recovered_sample_elapsed_seconds_are_checked(elapsed_ms: i64, refused: bool) {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        time: ReportedTime::CrashedSample(elapsed_ms),
        cancel: input.cancel.clone(),
        starts: std::rc::Rc::default(),
    });
    let ended = run(input);
    let last = ended
        .journal
        .events()
        .iter()
        .find_map(|event| {
            if let Payload::TrialEnd(end) = &event.payload {
                Some(end.last_sample_s)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        last,
        if refused {
            None
        } else {
            Some(u32::try_from(elapsed_ms / 1000).unwrap())
        }
    );
    let warning = ended.journal.events().iter().any(|event| {
        matches!(
            &event.payload,
            Payload::SessionWarning(warning) if warning.operation == "read conditions samples"
                && warning.error.contains("elapsed")
        )
    });
    assert_eq!(warning, refused);
}

#[test]
fn an_overflowing_teardown_mce_keeps_the_containment_dead_end_durable() {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    let started = std::rc::Rc::new(std::cell::Cell::new(false));
    let visible = std::rc::Rc::clone(&started);
    input.on_append = Some(Box::new(move |event| {
        if matches!(&event.payload, Payload::TrialStart(_)) {
            started.set(true);
        }
    }));
    input.machine.kernel = Box::new(MonotonicKernel {
        inner: input.machine.kernel,
        mono: std::time::Duration::from_nanos(u64::MAX) + std::time::Duration::from_nanos(1),
        visible,
    });
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        time: ReportedTime::EscapedOverflow,
        cancel: input.cancel.clone(),
        starts: std::rc::Rc::default(),
    });
    let ended = run(input);
    assert!(
        ended
            .errors
            .iter()
            .any(|error| matches!(error, togi_session::SessionError::Monotonic(_)))
    );
    assert!(ended.journal.events().iter().any(|event| matches!(
        &event.payload,
        Payload::DeadEnd(dead) if dead.condition == togi_journal::DeadEndCondition::Containment
            && dead.detail.contains("backend thread on cpu 63 outside allowed cpus")
    )));
    assert!(
        !ended
            .journal
            .events()
            .iter()
            .any(|event| matches!(&event.payload, Payload::TrialEnd(_)))
    );
}

struct TeardownClock {
    inner: Box<dyn togi_machine::Clock>,
    refuse: std::rc::Rc<std::cell::Cell<bool>>,
}

impl togi_machine::Clock for TeardownClock {
    fn now(&self) -> jiff::Timestamp {
        self.inner.now()
    }

    fn monotonic(&self) -> Result<std::time::Duration, togi_machine::Error> {
        if self.refuse.replace(false) {
            return togi_machine::monotonic_duration(-1, 0);
        }
        self.inner.monotonic()
    }

    fn sleep(
        &mut self,
        cancel: &CancellationToken,
        duration: std::time::Duration,
    ) -> Result<(), togi_machine::Error> {
        self.inner.sleep(cancel, duration)
    }
}

#[test]
fn a_refused_teardown_clock_keeps_the_containment_dead_end_durable() {
    let sim = simulator();
    let mem = MemFs::new();
    let mut input = input(&sim, &mem);
    let refuse = std::rc::Rc::new(std::cell::Cell::new(false));
    input.machine.kernel = Box::new(TeardownKernel {
        inner: input.machine.kernel,
        cancel: input.cancel.clone(),
        refuse: std::rc::Rc::clone(&refuse),
    });
    input.machine.clock = Box::new(TeardownClock {
        inner: input.machine.clock,
        refuse: std::rc::Rc::clone(&refuse),
    });
    input.machine.trials = Box::new(DurationTrials {
        inner: input.machine.trials,
        time: ReportedTime::EscapedOverflow,
        cancel: input.cancel.clone(),
        starts: std::rc::Rc::default(),
    });
    let ended = run(input);
    assert!(
        ended.errors.iter().any(|error| matches!(
            error,
            togi_session::SessionError::Machine(togi_machine::Error::InvalidMonotonic { .. })
        )),
        "{:?}",
        ended.errors
    );
    assert!(ended.journal.events().iter().any(|event| matches!(
        &event.payload,
        Payload::DeadEnd(dead) if dead.condition == togi_journal::DeadEndCondition::Containment
            && dead.detail.contains("backend thread on cpu 63 outside allowed cpus")
    )));
    assert!(
        !ended
            .journal
            .events()
            .iter()
            .any(|event| matches!(&event.payload, Payload::TrialEnd(_)))
    );
}

struct TeardownKernel {
    inner: Box<dyn togi_machine::Kernel>,
    cancel: CancellationToken,
    refuse: std::rc::Rc<std::cell::Cell<bool>>,
}

impl togi_machine::Kernel for TeardownKernel {
    fn mces(
        &mut self,
        boot: &togi_machine::BootId,
        since: std::time::Duration,
    ) -> Result<Vec<togi_machine::Mce>, togi_machine::Partial<Vec<togi_machine::Mce>>> {
        self.inner.mces(boot, since)
    }

    fn read_mces(
        &mut self,
        boot: &togi_machine::BootId,
        after: Option<&togi_machine::KernelCursor>,
    ) -> Result<togi_machine::KernelRead, togi_machine::Partial<togi_machine::KernelRead>> {
        let read = self.inner.read_mces(boot, after)?;
        if self.cancel.is_cancelled() {
            self.refuse.set(true);
        }
        Ok(read)
    }

    fn reset_reason(
        &mut self,
        boot: &togi_machine::BootId,
    ) -> Result<togi_machine::ResetReason, togi_machine::Error> {
        self.inner.reset_reason(boot)
    }

    fn reset_reason_after(
        &mut self,
        boot: &togi_machine::BootId,
    ) -> Result<togi_machine::ResetReason, togi_machine::Error> {
        self.inner.reset_reason_after(boot)
    }

    fn saved_pstore(
        &mut self,
        boot: &togi_machine::BootId,
    ) -> Result<Option<togi_machine::PstoreRecord>, togi_machine::Error> {
        self.inner.saved_pstore(boot)
    }
}
