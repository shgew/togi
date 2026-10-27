//! The public seams end to end: the runtime thread, the sync API and abandoned handles. A manual
//! clock bounds the synchronous waits and pauses the runtime thread's clock. Tests that act while a
//! trial runs keep the system clock, with trials far longer than the test.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use togi_backend::{Backend, Line, Plan, Ready};
use togi_journal::fs::{FileKind, Fs, MemFs};
use togi_machine::{
    CancellationToken, CoreId, CpuId, Partial, PmTable, PmTableReader, Regime, Reporter, Running,
    Sample, Signal, Stopped, TrialResult, Trials, Workload,
};

use super::harness::{
    Run, crate_error, finish, identity_of, local, nothing_runs, printing, rig, rig_edit, spec,
    topology,
};
use crate::clock::{Clock, Manual};
use crate::error::{CleanupFailure, Error, InvalidTrial};
use crate::id::ScopeName;
use crate::native::Settings;
use crate::os::fake::{Delivery, Fake, Launch, ProcCtx, Reply, script};
use crate::os::{ExitInfo, GroupSignal, KillSignal, Systemd};
use crate::runner::{Backends, Options, Runner};
use crate::sensors::Roots;
use crate::supervisor::{Event, Finished, TrialCommand};
use crate::testing::TestBackend;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn fast() -> Timing {
    Timing {
        sample: Duration::from_millis(50),
        stop_grace: Duration::from_millis(100),
        teardown: Duration::from_millis(800),
        kill_slot: Duration::from_millis(200),
        drain: Duration::from_millis(300),
        writer_join: Duration::from_millis(500),
        probe: Duration::from_millis(300),
        margin: Duration::from_millis(200),
        ..Timing::default()
    }
}

/// A runner on a manual clock: the runtime thread's clock is paused, and a synchronous wait ends
/// only when it is answered.
fn runner(setup: impl FnOnce(&Fake) + Send + 'static, timing: Timing) -> (Runner, Arc<MemFs>) {
    runner_on(Clock::Manual(Manual::new()), setup, timing)
}

/// A runner on the system clock, for tests that act while a trial of ten minutes runs.
fn live_runner(setup: impl FnOnce(&Fake) + Send + 'static, timing: Timing) -> (Runner, Arc<MemFs>) {
    runner_on(Clock::System, setup, timing)
}

fn runner_on(
    clock: Clock,
    setup: impl FnOnce(&Fake) + Send + 'static,
    timing: Timing,
) -> (Runner, Arc<MemFs>) {
    let fs = Arc::new(MemFs::default());
    let root = Path::new("/rig");
    let memory = crate::watch::Memory::new(fs.clone());
    let options = Options {
        dir: root.join("trials"),
        backends: Backends::new().with(Arc::new(TestBackend::new("/bin/true"))),
        topology: topology(),
        identity: Some(identity_of(1000, 1000)),
        pm_table: None,
    };
    let roots = Roots {
        hwmon: root.join("hwmon"),
        cpu: root.join("cpu"),
        powercap: root.join("powercap"),
        proc: root.join("proc"),
    };
    let settings = Settings {
        timing,
        roots,
        unscoped: false,
        user_manager: false,
        controller: Arc::new(crate::controller::Recording::default()),
        frequency: super::harness::frequency(&topology()),
        fs: Some(fs.clone()),
        inputs: crate::procfs::Inputs::memory(fs.clone()),
        memory: Some(memory.clone()),
        durability: crate::samples::Durability::Unsynced,
        clock,
    };
    let runner = Runner::build(options, settings, move || {
        let fake = Fake::with_memory(memory);
        setup(&fake);
        fake
    })
    .unwrap();
    (runner, fs)
}

#[derive(Default)]
struct Collect {
    progress: Vec<String>,
    signals: Vec<(CoreId, Signal)>,
    signal_pids: Vec<Option<u32>>,
}

impl Reporter for Collect {
    fn progress(&mut self, detail: &str) {
        self.progress.push(detail.to_owned());
    }

    fn sample(&mut self, _: Sample) {}

    fn signal(&mut self, core: CoreId, signal: Signal, pid: Option<u32>, _: &str) {
        self.signals.push((core, signal));
        self.signal_pids.push(pid);
    }
}

fn start(
    runner: &mut Runner,
    regime: Regime,
    cores: &[u8],
    duration: Duration,
) -> Box<dyn Running> {
    runner
        .start(
            &CancellationToken::new(),
            spec(regime, cores, duration),
            nothing_runs(),
        )
        .map_err(|partial| partial.error)
        .unwrap()
}

fn wait_for(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

/// A worker that records when it is asked to terminate, then ends.
fn terminating(terminated: &Arc<AtomicBool>) -> Launch {
    let terminated = Arc::clone(terminated);
    Launch::Run(script(move |mut ctx: ProcCtx| async move {
        while let Some(delivery) = ctx.signals.recv().await {
            if matches!(delivery, Delivery::Group(GroupSignal::Term)) {
                terminated.store(true, Ordering::SeqCst);
                ctx.exit(ExitInfo::Signal(15));
                return;
            }
        }
    }))
}

fn marks_termination(terminated: Arc<AtomicBool>) -> impl FnOnce(&Fake) + Send + 'static {
    move |fake| {
        fake.on_launch(move |_, _| terminating(&terminated));
    }
}

/// A runner whose workers record termination and whose first launch holds the runtime thread, as
/// slow filesystem or launcher setup does: it lets the start's wait run out on `clock`, then holds
/// until `release` answers. `ready` is whether the workers start inside their scope.
struct Stalled {
    runner: Runner,
    clock: Arc<Manual>,
    release: std::sync::mpsc::Sender<()>,
    terminated: Arc<AtomicBool>,
    launches: Arc<AtomicUsize>,
}

fn stalled(ready: bool) -> Stalled {
    let clock = Manual::new();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let terminated = Arc::new(AtomicBool::new(false));
    let launches = Arc::new(AtomicUsize::new(0));
    let setup = {
        let (clock, terminated, launches) = (
            Arc::clone(&clock),
            Arc::clone(&terminated),
            Arc::clone(&launches),
        );
        move |fake: &Fake| {
            fake.set_ready_default(ready);
            fake.on_launch(move |_, _| {
                if launches.fetch_add(1, Ordering::SeqCst) == 0 {
                    clock.expire_waiting();
                    let _ = held.recv();
                }
                terminating(&terminated)
            });
        }
    };
    let (runner, _fs) = runner_on(Clock::Manual(Arc::clone(&clock)), setup, fast());
    Stalled {
        runner,
        clock,
        release,
        terminated,
        launches,
    }
}

#[test]
fn a_whole_trial_runs_waits_and_stops_through_the_public_seams() {
    let (mut runner, fs) = runner(
        |fake| {
            fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.print("ok one\nok two\n").await;
                    ctx.until_killed().await;
                }))
            });
        },
        fast(),
    );
    let mut trial = start(&mut runner, Regime::R1, &[2], Duration::from_millis(300));
    assert_eq!(trial.started().instances[0].scope, "togi-trial-t1");
    let mut report = Collect::default();
    let result = trial
        .wait(&CancellationToken::new(), &mut report)
        .map_err(|partial| partial.error)
        .unwrap();
    assert_eq!(report.progress, ["ok one", "ok two"]);
    assert_eq!(result.failure, None);
    assert_eq!(result.ran, Duration::from_millis(300));
    let _stopped: Stopped = trial.stop().unwrap();
    assert!(fs.contents("/rig/trials/t1/work/stdout.log").is_some());
    assert_eq!(
        fs.contents("/rig/trials/t1/work/stdout.log").unwrap(),
        b"ok one\nok two\n"
    );
}

#[test]
fn a_failing_trial_reports_its_signal_on_the_waiting_thread() {
    let (mut runner, _dir) = runner(
        |fake| {
            fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.print("COMPUTE ERROR\n").await;
                    ctx.until_killed().await;
                }))
            });
        },
        fast(),
    );
    let mut trial = start(&mut runner, Regime::R1, &[1], secs(60));
    let mut report = Collect::default();
    let result = trial.wait(&CancellationToken::new(), &mut report).unwrap();
    assert_eq!(report.signals.len(), 1);
    let expected_pid = Some(trial.started().instances[0].pid);
    assert_eq!(report.signal_pids, vec![expected_pid]);
    assert_eq!(
        result
            .failure
            .and_then(|failure| failure.pid)
            .map(std::num::NonZeroU32::get),
        expected_pid
    );
    assert_eq!(
        result.failure.map(|f| f.signal),
        Some(Signal::ComputationError)
    );
    assert!(trial.stop().is_ok());
}

#[test]
fn stopping_without_waiting_tears_down_and_proves_it() {
    let (mut runner, _dir) = live_runner(|_| {}, fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    assert!(trial.stop().is_ok());
}

#[test]
fn readers_keep_draining_between_start_and_wait() {
    let (mut runner, fs) = live_runner(
        |fake| {
            fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    for _ in 0..200 {
                        ctx.print(&"ok line of output\n".repeat(100)).await;
                    }
                    ctx.until_killed().await;
                }))
            });
        },
        fast(),
    );
    let mut trial = start(&mut runner, Regime::R1, &[0], secs(600));
    let log = Path::new("/rig/trials/t1/work/stdout.log");
    let expected = 200 * 100 * "ok line of output\n".len();
    assert!(
        wait_for(secs(10), || fs
            .contents(log)
            .is_some_and(|bytes| bytes.len() == expected)),
        "the log filled before anyone waited"
    );
    let mut report = Collect::default();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let outcome = trial.wait(&cancel, &mut report);
    assert!(matches!(
        outcome,
        Err(Partial {
            error: togi_machine::Error::Cancelled,
            ..
        })
    ));
    assert_eq!(
        report.progress.len(),
        200 * 100,
        "every queued progress line reaches the waiter"
    );
    assert!(trial.stop().is_ok());
}

#[test]
fn the_blocking_apis_refuse_to_run_inside_a_runtime() {
    crate::testing::runtime(false).block_on(async {
        let (mut runner, _dir) = runner(|_| {}, fast());
        assert!(matches!(
            runner.probe_scope(),
            Err(Error::InsideRuntime {
                operation: "the scope probe"
            })
        ));
        let sweep = runner.sweep(&CancellationToken::new()).unwrap_err();
        assert!(matches!(
            crate_error(&sweep),
            Some(Error::InsideRuntime {
                operation: "the sweep"
            })
        ));
        let partial = runner
            .start(
                &CancellationToken::new(),
                spec(Regime::R1, &[0], secs(1)),
                nothing_runs(),
            )
            .err()
            .unwrap();
        assert!(partial.value.is_some(), "the proof comes back untouched");
        assert!(matches!(
            crate_error(&partial.error),
            Some(Error::InsideRuntime { operation: "start" })
        ));
        assert!(runner.ended("t1").is_err());
    });
}

#[test]
fn waiting_and_stopping_inside_a_runtime_are_refused_and_the_trial_stays_usable() {
    let (mut runner, _dir) = runner(|_| {}, fast());
    let mut trial = start(&mut runner, Regime::R1, &[0], Duration::from_millis(200));
    let runtime = crate::testing::runtime(false);
    let refused = runtime.block_on(async {
        trial
            .wait(&CancellationToken::new(), &mut Collect::default())
            .err()
            .map(|partial| partial.error)
    });
    assert!(matches!(
        refused.as_ref().and_then(crate_error),
        Some(Error::InsideRuntime { operation: "wait" })
    ));
    let stop_refused = runtime.block_on(async { trial_stop_refused(trial) });
    assert!(stop_refused);
}

fn trial_stop_refused(trial: Box<dyn Running>) -> bool {
    matches!(
        trial.stop().as_ref().map_err(crate_error),
        Err(Some(Error::InsideRuntime { operation: "stop" }))
    )
}

#[test]
fn a_trial_dropped_after_start_is_torn_down_by_its_supervisor() {
    let terminated = Arc::new(AtomicBool::new(false));
    let (mut runner, _keep) = live_runner(marks_termination(Arc::clone(&terminated)), fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    drop(trial);
    assert!(
        wait_for(secs(10), || terminated.load(Ordering::SeqCst)),
        "SIGTERM reached the abandoned workload"
    );
}

#[test]
fn a_trial_dropped_while_supervision_runs_is_torn_down_too() {
    let terminated = Arc::new(AtomicBool::new(false));
    let (mut runner, fs) = live_runner(marks_termination(Arc::clone(&terminated)), fast());
    let trial = start(&mut runner, Regime::R7, &[0, 1], secs(600));
    assert!(
        wait_for(secs(10), || fs
            .contents("/rig/trials/t1/samples.jsonl")
            .is_some_and(|samples| !samples.is_empty())),
        "supervision took a sample"
    );
    drop(trial);
    assert!(wait_for(secs(10), || terminated.load(Ordering::SeqCst)));
}

#[test]
fn a_trial_dropped_after_wait_changes_nothing() {
    let (mut runner, _dir) = runner(|_| {}, fast());
    let mut trial = start(&mut runner, Regime::R1, &[0], Duration::from_millis(200));
    let result = trial.wait(&CancellationToken::new(), &mut Collect::default());
    assert!(result.is_ok());
    drop(trial);
    let next = start(&mut runner, Regime::R1, &[0], Duration::from_millis(100));
    assert!(next.stop().is_ok());
}

/// Cancels the wait it reports to at the first progress line.
struct CancelOnProgress(CancellationToken);

impl Reporter for CancelOnProgress {
    fn progress(&mut self, _: &str) {
        self.0.cancel();
    }

    fn sample(&mut self, _: Sample) {}

    fn signal(&mut self, _: CoreId, _: Signal, _: Option<u32>, _: &str) {}
}

#[test]
fn cancelling_a_wait_returns_a_partial_result_and_stop_still_proves_the_teardown() {
    let (mut runner, _dir) = live_runner(
        |fake| {
            fake.on_launch(printing("ok started\n"));
        },
        fast(),
    );
    let mut trial = start(&mut runner, Regime::R1, &[0], secs(600));
    let cancel = CancellationToken::new();
    let partial = trial
        .wait(&cancel, &mut CancelOnProgress(cancel.clone()))
        .unwrap_err();
    assert!(matches!(partial.error, togi_machine::Error::Cancelled));
    assert!(partial.value.ran < secs(600));
    assert!(trial.stop().is_ok());
}

#[test]
fn an_unconfirmed_teardown_is_containment_for_wait_and_for_stop() {
    let (mut runner, _dir) = runner(
        |fake| {
            fake.on_systemd(|call| {
                if let Systemd::Kill { .. } = call {
                    Some(Reply::Hang)
                } else {
                    None
                }
            });
        },
        fast(),
    );
    let mut trial = start(&mut runner, Regime::R1, &[0], Duration::from_millis(200));
    let partial = trial
        .wait(&CancellationToken::new(), &mut Collect::default())
        .unwrap_err();
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
    assert_eq!(
        partial.value.failure, None,
        "evidence is kept beside the containment"
    );
    assert!(matches!(
        trial.stop(),
        Err(togi_machine::Error::Containment(_))
    ));
}

#[test]
fn dropping_the_runner_is_bounded_even_with_a_stuck_trial() {
    let (mut runner, _dir) = runner(
        |fake| {
            fake.on_launch(|_, _| {
                Launch::Run(script(|ctx: ProcCtx| async move {
                    let _keep = ctx;
                    std::future::pending::<()>().await;
                }))
            });
            fake.on_systemd(|call| {
                if let Systemd::Kill { .. } = call {
                    Some(Reply::Hang)
                } else {
                    None
                }
            });
        },
        fast(),
    );
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    std::mem::forget(trial);
    drop(runner);
}

#[test]
fn dropping_the_runner_gives_up_on_a_wedged_runtime_thread_at_its_join_bound() {
    let Stalled {
        mut runner,
        clock,
        release: _held,
        ..
    } = stalled(true);
    let partial = start_failure(&mut runner, Regime::R1, &[0], secs(600));
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
    let begun = clock.elapsed();
    let expiry = std::thread::spawn({
        let clock = Arc::clone(&clock);
        move || clock.expire_waiting()
    });
    drop(runner);
    expiry.join().unwrap();
    assert_eq!(clock.elapsed(), begun + fast().stop_bound() + secs(1));
}

#[test]
fn dropping_the_runner_tears_down_a_trial_whose_handle_was_never_dropped() {
    let terminated = Arc::new(AtomicBool::new(false));
    let (mut runner, _keep) = live_runner(marks_termination(Arc::clone(&terminated)), fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    std::mem::forget(trial);
    drop(runner);
    assert!(
        terminated.load(Ordering::SeqCst),
        "the supervisor terminated its workload before the runner ended"
    );
}

#[test]
fn the_runner_sweeps_probes_compresses_and_reads_samples() {
    let (mut runner, fs) = runner(
        |fake| {
            fake.add_unit("togi-trial-old.scope");
        },
        fast(),
    );
    assert_eq!(
        runner.probe_scope().unwrap(),
        "scope confined to cpu 0 created"
    );
    let swept = runner.sweep(&CancellationToken::new()).unwrap();
    assert!(swept.detail.starts_with("removed 1 leftover trial scopes"));

    let mut trial = start(&mut runner, Regime::R1, &[0], Duration::from_millis(300));
    let _ = trial.wait(&CancellationToken::new(), &mut Collect::default());
    drop(trial.stop().unwrap());
    runner.ended("t1").unwrap();
    assert!(fs.contents("/rig/trials/t1/work/stdout.log.gz").is_some());
    assert!(fs.contents("/rig/trials/t1/work/stdout.log").is_none());
    let samples = runner.samples("t1").collect::<Result<Vec<_>, _>>().unwrap();
    assert!(!samples.is_empty(), "the trial's samples read back");
    // An entry the injected filesystem cannot open as a file is reported, not read as empty.
    fs.create_dir(Path::new("/rig/trials/t2")).unwrap();
    fs.create_dir(Path::new("/rig/trials/t2/samples.jsonl"))
        .unwrap();
    let mut unreadable = runner.samples("t2");
    let first = unreadable.next();
    assert!(
        matches!(first, Some(Err(togi_machine::SampleError::Open(_)))),
        "{first:?}"
    );
    assert!(unreadable.next().is_none());
    drop(unreadable);
    assert!(runner.samples("../t1").next().unwrap().is_err());
    assert!(runner.ended("../t1").is_err());
}

#[test]
fn a_cancelled_sweep_returns_cancelled_without_waiting_out_its_deadline() {
    let cancel = CancellationToken::new();
    let (mut runner, _dir) = runner(
        {
            let cancel = cancel.clone();
            move |fake| {
                fake.on_systemd(move |call| {
                    if let Systemd::ListScopes = call {
                        cancel.cancel();
                        Some(Reply::Hang)
                    } else {
                        None
                    }
                });
            }
        },
        Timing::default(),
    );
    let error = runner.sweep(&cancel).unwrap_err();
    assert!(matches!(error, togi_machine::Error::Cancelled));
}

#[test]
fn a_trial_longer_than_the_teardown_budgets_still_completes_through_wait() {
    let timing = fast();
    let (mut runner, _dir) = runner(|_| {}, timing);
    let long = timing.teardown + timing.writer_join + timing.margin + Duration::from_secs(1);
    let mut trial = start(&mut runner, Regime::R1, &[0], long);
    let result = trial
        .wait(&CancellationToken::new(), &mut Collect::default())
        .map_err(|partial| partial.error)
        .unwrap();
    assert_eq!(result.ran, long);
    assert!(trial.stop().is_ok());
}

/// A trial whose supervisor's events are what the test sends, on `clock`.
fn scripted_supervisor(
    duration: Duration,
    timing: &Timing,
    clock: &Arc<Manual>,
) -> (crate::native::Trial, std::sync::mpsc::Sender<Event>) {
    let (events_tx, events) = std::sync::mpsc::channel();
    let (commands, _commands_rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = crate::supervisor::Handle {
        commands,
        events,
        started: togi_machine::Started::default(),
        duration,
        cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        registration: CancellationToken::new().on_cancel(|| {}),
    };
    let trial = crate::native::Trial::new(handle, timing, Clock::Manual(Arc::clone(clock)));
    (trial, events_tx)
}

fn finished(ran: Duration) -> Event {
    Event::Finished(Box::new(Finished {
        result: TrialResult {
            ran,
            ..TrialResult::default()
        },
        errors: Vec::new(),
        teardown: Ok(nothing_runs()),
        abandoned: false,
    }))
}

/// A supervisor bound of 300 ms past the trial's duration.
fn short_bound() -> Timing {
    Timing {
        teardown: Duration::from_millis(100),
        writer_join: Duration::from_millis(100),
        margin: Duration::from_millis(100),
        ..Timing::default()
    }
}

#[test]
fn wait_gives_up_on_a_supervisor_that_never_answers_at_its_bound() {
    let timing = short_bound();
    let clock = Manual::new();
    let (mut trial, _kept_open) = scripted_supervisor(Duration::from_millis(200), &timing, &clock);
    let expiry = std::thread::spawn({
        let clock = Arc::clone(&clock);
        move || clock.expire_waiting()
    });
    let partial = trial
        .wait(&CancellationToken::new(), &mut Collect::default())
        .unwrap_err();
    expiry.join().unwrap();
    assert_eq!(clock.elapsed(), Duration::from_millis(500));
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
    assert_eq!(partial.value, TrialResult::default());
}

#[test]
fn stop_gives_up_on_a_supervisor_that_never_answers_with_containment() {
    let timing = short_bound();
    let clock = Manual::new();
    let (trial, _kept_open) = scripted_supervisor(secs(60), &timing, &clock);
    let expiry = std::thread::spawn({
        let clock = Arc::clone(&clock);
        move || clock.expire_waiting()
    });
    let outcome = Box::new(trial).stop();
    expiry.join().unwrap();
    assert!(matches!(outcome, Err(togi_machine::Error::Containment(_))));
    assert_eq!(clock.elapsed(), Duration::from_millis(300));
}

#[test]
fn the_wait_bound_covers_the_whole_duration_of_a_long_trial() {
    let timing = Timing::default();
    let (trial, _kept_open) = scripted_supervisor(secs(900), &timing, &Manual::new());
    let remaining = trial.wait_budget();
    assert!(remaining > secs(900) + timing.teardown + timing.writer_join);
}

/// A reporter that takes 10 ms of `clock` over each progress line it counts.
struct Slow {
    clock: Arc<Manual>,
    progress: usize,
}

impl Slow {
    fn on(clock: &Arc<Manual>) -> Self {
        Self {
            clock: Arc::clone(clock),
            progress: 0,
        }
    }
}

impl Reporter for Slow {
    fn progress(&mut self, _: &str) {
        self.clock.advance(Duration::from_millis(10));
        self.progress += 1;
    }

    fn sample(&mut self, _: Sample) {}

    fn signal(&mut self, _: CoreId, _: Signal, _: Option<u32>, _: &str) {}
}

#[test]
fn a_backlog_of_reports_cannot_carry_wait_past_its_absolute_bound() {
    let timing = short_bound();
    let clock = Manual::new();
    let (mut trial, events) = scripted_supervisor(Duration::from_millis(200), &timing, &clock);
    for _ in 0..1000 {
        events.send(Event::Progress("ok".into())).unwrap();
    }
    let mut slow = Slow::on(&clock);
    let outcome = trial.wait(&CancellationToken::new(), &mut slow);
    assert!(outcome.is_err());
    assert_eq!(clock.elapsed(), Duration::from_millis(500));
    assert_eq!(slow.progress, 50);
}

#[test]
fn containment_carries_the_ordered_cleanup_failures_as_its_source() {
    let (mut runner, _dir) = runner(
        |fake| {
            fake.on_systemd(|call| {
                if let Systemd::Kill { .. } = call {
                    Some(Reply::Hang)
                } else {
                    None
                }
            });
        },
        fast(),
    );
    let trial = start(&mut runner, Regime::R1, &[0], Duration::from_millis(100));
    let Err(togi_machine::Error::Containment(source)) = trial.stop() else {
        panic!("not containment");
    };
    let containment = source
        .downcast_ref::<Arc<crate::Containment>>()
        .expect("the source is the failure list");
    assert!(matches!(
        containment.failures().first(),
        Some(crate::CleanupFailure::KillScope { .. })
    ));
}

fn start_failure(
    runner: &mut Runner,
    regime: Regime,
    cores: &[u8],
    duration: Duration,
) -> Partial<Option<Stopped>> {
    runner
        .start(
            &CancellationToken::new(),
            spec(regime, cores, duration),
            nothing_runs(),
        )
        .err()
        .expect("the start fails")
}

#[test]
fn a_stalled_start_is_containment_without_a_value_and_the_trial_it_finishes_late_is_abandoned() {
    let mut stall = stalled(true);
    let partial = start_failure(&mut stall.runner, Regime::R1, &[0], secs(600));
    assert_eq!(stall.clock.elapsed(), fast().start_bound(1));
    assert!(partial.value.is_none(), "no proof that nothing runs");
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
    stall.release.send(()).unwrap();
    assert!(
        wait_for(secs(10), || stall.terminated.load(Ordering::SeqCst)),
        "the workload of a start nobody waits for was torn down"
    );
}

#[test]
fn a_timed_out_start_launches_no_further_instance_and_rolls_back_the_first() {
    let mut stall = stalled(true);
    let partial = start_failure(&mut stall.runner, Regime::R7, &[0, 1], secs(600));
    assert!(partial.value.is_none());
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
    stall.release.send(()).unwrap();
    assert!(
        wait_for(secs(10), || stall.terminated.load(Ordering::SeqCst)),
        "the first instance was rolled back"
    );
    assert_eq!(
        stall.launches.load(Ordering::SeqCst),
        1,
        "a start nobody waits for launches nothing more"
    );
}

#[test]
fn dropping_the_runner_after_a_timed_out_start_waits_for_the_rollback_in_progress() {
    let Stalled {
        mut runner,
        clock,
        release,
        terminated,
        ..
    } = stalled(false);
    let partial = start_failure(&mut runner, Regime::R6, &[0], secs(600));
    assert!(partial.value.is_none());
    let released = std::thread::spawn(move || {
        clock.until_waiting();
        release.send(()).unwrap();
    });
    drop(runner);
    released.join().unwrap();
    assert!(
        terminated.load(Ordering::SeqCst),
        "shutdown let the start that was waiting for its scope roll its workload back"
    );
}

#[test]
fn dropping_the_trial_and_the_runner_back_to_back_still_terminates_the_workload() {
    let terminated = Arc::new(AtomicBool::new(false));
    let (mut runner, _keep) = live_runner(marks_termination(Arc::clone(&terminated)), fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    drop(trial);
    drop(runner);
    assert!(
        terminated.load(Ordering::SeqCst),
        "the runner ended only after the abandoned workload was terminated"
    );
}

#[test]
fn a_duration_that_cannot_be_scheduled_is_refused_before_anything_changes() {
    let (mut runner, fs) = runner(|_| {}, fast());
    let partial = start_failure(&mut runner, Regime::R1, &[0], Duration::MAX);
    assert!(partial.value.is_some(), "nothing ran, so the proof returns");
    assert!(matches!(
        crate_error(&partial.error),
        Some(Error::Invalid(InvalidTrial::UnrepresentableDuration))
    ));
    assert_eq!(fs.kind(Path::new("/rig/trials")).unwrap(), None);
}

#[test]
fn a_repeated_wait_reports_the_failure_the_first_one_did() {
    let (mut runner, _dir) = live_runner(|_| {}, fast());
    let mut trial = start(&mut runner, Regime::R1, &[0], secs(600));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let first = trial.wait(&cancel, &mut Collect::default()).unwrap_err();
    assert!(matches!(first.error, togi_machine::Error::Cancelled));
    let again = trial
        .wait(&CancellationToken::new(), &mut Collect::default())
        .unwrap_err();
    assert!(matches!(again.error, togi_machine::Error::Cancelled));
    assert_eq!(again.value, first.value);
    assert!(trial.stop().is_ok());
}

fn launch_error() -> Error {
    Error::Launch {
        instance: "c01".into(),
        scope: "togi-trial-t1-c01".into(),
        source: std::io::Error::other("systemd-run missing"),
    }
}

fn joined_containment_error() -> Error {
    let containment = crate::Containment::new(CleanupFailure::Reap {
        instance: "c00".into(),
        source: std::io::Error::other("the leader did not exit"),
    })
    .caused_by(launch_error());
    Error::join_after(
        Error::Containment(std::sync::Arc::new(containment)),
        vec![Error::Cancelled],
    )
}

#[test_case::test_case(launch_error(), "wait for the trial: launch instance c01 in scope togi-trial-t1-c01: systemd-run missing"; "launch")]
#[test_case::test_case(joined_containment_error(), "trial cleanup could not be confirmed: wait for the trial: trial cleanup could not be confirmed: the start failed: launch instance c01 in scope togi-trial-t1-c01: systemd-run missing; reap instance c00: the leader did not exit; cancelled"; "joined containment")]
fn shared_errors_render_each_cause_once(error: Error, expected: &str) {
    let error = crate::error::Shared::seam(&std::sync::Arc::new(error));
    assert_eq!(togi_machine::error_chain(&error), expected);
}

#[test_case::test_case(launch_error(), true; "ordinary")]
#[test_case::test_case(Error::Stuck { operation: "stop", bound: Duration::from_secs(1) }, false; "containment")]
fn a_kept_joined_error_is_reachable_through_the_public_source_chain(error: Error, is_other: bool) {
    let error = Error::join_after(error, vec![Error::Cancelled]);
    let seam = crate::error::Shared::seam(&Arc::new(error));
    let (togi_machine::Error::Other(inner) | togi_machine::Error::Containment(inner)) = &seam
    else {
        panic!("{seam:?}");
    };
    assert_eq!(matches!(seam, togi_machine::Error::Other(_)), is_other);
    let kept = std::iter::successors(
        Some(&**inner as &(dyn std::error::Error + 'static)),
        |error| error.source(),
    )
    .find_map(|error| error.downcast_ref::<Error>());
    assert!(matches!(kept, Some(Error::Joined(_))), "{seam:?}");
}

#[test]
fn a_stuck_error_renders_its_cause_once() {
    let stuck = Error::Stuck {
        operation: "stop",
        bound: Duration::from_secs(1),
    };
    let seam = crate::error::Shared::seam(&Arc::new(stuck));
    assert_eq!(
        togi_machine::error_chain(&seam),
        "trial cleanup could not be confirmed: wait for the trial: \
         the trial runtime did not answer stop within 1s"
    );
}

#[test]
fn a_cause_whose_text_is_part_of_another_is_rendered_each_time_it_occurs() {
    let reap = |detail: &str| CleanupFailure::Reap {
        instance: "c00".into(),
        source: std::io::Error::other(detail.to_owned()),
    };
    let mut containment = crate::Containment::new(reap("gone"));
    containment.push(reap("gone"));
    containment.push(reap("gone for good"));
    assert_eq!(
        togi_machine::error_chain(&containment),
        "reap instance c00: gone; reap instance c00: gone; reap instance c00: gone for good"
    );
    let joined = Error::join_after(
        Error::Signal {
            instance: "c00".into(),
            signal: "SIGTERM",
            source: std::io::Error::other("signal"),
        },
        vec![Error::Signal {
            instance: "c00".into(),
            signal: "SIGTERM",
            source: std::io::Error::other("signal"),
        }],
    );
    assert_eq!(
        togi_machine::error_chain(&joined),
        "signal instance c00 with SIGTERM: signal; signal instance c00 with SIGTERM: signal"
    );
}

#[test]
fn a_kept_error_names_its_operation_and_keeps_the_error_as_its_source() {
    let kept = Arc::new(Error::Launch {
        instance: "c01".into(),
        scope: "togi-trial-t1-c01".into(),
        source: std::io::Error::other("systemd-run missing"),
    });
    let seam = crate::error::Shared::seam(&kept);
    let togi_machine::Error::Other(inner) = &seam else {
        panic!("{seam:?}");
    };
    assert_eq!(inner.to_string(), "wait for the trial");
    let source = inner.source().expect("the kept error is the source");
    assert!(matches!(
        source.downcast_ref::<Error>(),
        Some(Error::Launch { .. })
    ));
    assert!(std::ptr::eq(
        source.downcast_ref::<Error>().unwrap(),
        std::ptr::from_ref(kept.as_ref())
    ));
    let io = source
        .source()
        .and_then(|cause| cause.downcast_ref::<std::io::Error>())
        .expect("the io cause stays downcastable");
    assert_eq!(io.to_string(), "systemd-run missing");
}

#[test]
fn storage_failures_list_each_member_with_its_own_causes() {
    use crate::error::{StorageError, StorageFailure};
    let failure = StorageFailure(vec![
        StorageError::Append(std::io::Error::other("sync failed")),
        StorageError::Close(std::io::Error::other("sync failed")),
    ]);
    assert_eq!(
        togi_machine::error_chain(&failure),
        "append a conditions sample: sync failed; close the samples file: sync failed"
    );
    let error = Error::from(failure);
    assert_eq!(
        togi_machine::error_chain(&error),
        "the samples file: append a conditions sample: sync failed; \
         close the samples file: sync failed"
    );
    let Error::Storage(kept) = &error else {
        panic!("{error:?}");
    };
    let io = kept.errors().iter().map(|member| {
        std::error::Error::source(member)
            .and_then(|cause| cause.downcast_ref::<std::io::Error>())
            .is_some()
    });
    assert!(io.eq([true, true]));
}

#[test]
fn a_cancelled_trial_keeps_the_output_failure_that_followed_its_cancellation() {
    let (mut runner, _dir) = live_runner(
        |fake| {
            fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    while let Some(delivery) = ctx.signals.recv().await {
                        if matches!(delivery, Delivery::Group(GroupSignal::Term)) {
                            ctx.print(&"x".repeat(70_000)).await;
                            ctx.exit(ExitInfo::Signal(15));
                            return;
                        }
                    }
                }))
            });
        },
        fast(),
    );
    let mut trial = start(&mut runner, Regime::R1, &[0], secs(600));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let waited = trial.wait(&cancel, &mut Collect::default());
    let Err(Partial {
        error: togi_machine::Error::Other(inner),
        ..
    }) = waited
    else {
        panic!("a cancellation that carries an output failure must not read as a bare one");
    };
    let Some(Error::Joined(joined)) = inner
        .source()
        .and_then(|source| source.downcast_ref::<Error>())
    else {
        panic!("{inner}");
    };
    assert!(
        matches!(joined.errors(), [Error::Cancelled, Error::Output(_)]),
        "{joined}"
    );
    assert!(trial.stop().is_ok());
}

/// Timings whose supervisor bound is short: a trial's wait bound is its duration plus 600 ms.
fn quick() -> Timing {
    Timing {
        sample: Duration::from_millis(50),
        stop_grace: Duration::from_millis(50),
        teardown: Duration::from_millis(300),
        kill_slot: Duration::from_millis(100),
        drain: Duration::from_millis(150),
        writer_join: Duration::from_millis(200),
        margin: Duration::from_millis(100),
        ..Timing::default()
    }
}

#[test]
fn a_first_wait_after_the_old_deadline_still_gets_the_finished_outcome() {
    let clock = Manual::new();
    let (mut trial, events) = scripted_supervisor(Duration::from_millis(100), &quick(), &clock);
    events.send(finished(Duration::from_millis(100))).unwrap();
    clock.advance(Duration::from_millis(1300));
    let result = trial
        .wait(&CancellationToken::new(), &mut Collect::default())
        .map_err(|partial| partial.error)
        .unwrap();
    assert_eq!(result.ran, Duration::from_millis(100));
    assert!(
        trial
            .wait(&CancellationToken::new(), &mut Collect::default())
            .is_ok(),
        "and the outcome stays retrievable"
    );
    assert!(Box::new(trial).stop().is_ok());
}

#[test]
fn a_late_wait_with_a_backlog_of_reports_is_still_bounded() {
    let clock = Manual::new();
    let (mut trial, events) = scripted_supervisor(Duration::from_millis(100), &quick(), &clock);
    for _ in 0..300 {
        events.send(Event::Progress("ok line".into())).unwrap();
    }
    events.send(finished(Duration::from_millis(100))).unwrap();
    clock.advance(Duration::from_millis(1300));
    let mut slow = Slow::on(&clock);
    let partial = trial
        .wait(&CancellationToken::new(), &mut slow)
        .unwrap_err();
    assert_eq!(clock.elapsed(), Duration::from_millis(1900));
    assert_eq!(slow.progress, 60, "reports stop at the stop bound");
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
}

fn names(fs: &dyn Fs, dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs
        .read_dir(dir)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name.into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Every file of every trial directory except `skip`, by path.
fn retained(fs: &dyn Fs, root: &Path, skip: &str) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    for trial in fs.read_dir(root).unwrap() {
        if trial.name == skip {
            continue;
        }
        let trial = root.join(trial.name);
        for file in fs.read_dir(&trial).unwrap() {
            let path = trial.join(file.name);
            found.insert(path.clone(), fs.read(&path).unwrap());
        }
    }
    found
}

fn files_below(fs: &dyn Fs, dir: &Path, prefix: &str, found: &mut Vec<String>) {
    for entry in fs.read_dir(dir).unwrap() {
        let name = format!("{prefix}{}", entry.name.to_str().unwrap());
        if entry.kind == FileKind::Dir {
            files_below(fs, &dir.join(&entry.name), &format!("{name}/"), found);
        } else {
            found.push(name);
        }
    }
    found.sort();
}

#[test]
fn ending_a_trial_compresses_it_and_leaves_every_older_retained_trial_untouched() {
    let (mut runner, fs) = runner(|_| {}, fast());
    let trials = Path::new("/rig/trials");
    for i in 0..205 {
        let old = trials.join(format!("{:04}", 9000 + i));
        fs.put(old.join("passed"), b"");
        fs.put(old.join("samples.jsonl.gz"), b"old");
    }
    let mut expected = names(fs.as_ref(), trials);
    expected.push("t1".to_owned());
    expected.sort();
    let before = retained(fs.as_ref(), trials, "t1");
    assert_eq!(before.len(), 410);

    let mut trial = start(&mut runner, Regime::R7, &[0, 1], Duration::from_millis(300));
    let _ = trial.wait(&CancellationToken::new(), &mut Collect::default());
    drop(trial.stop().unwrap());
    runner.ended("t1").unwrap();

    let mut files = Vec::new();
    files_below(fs.as_ref(), &trials.join("t1"), "", &mut files);
    let has = |file: &str, extension: &str| {
        Path::new(file)
            .extension()
            .is_some_and(|found| found == extension)
    };
    let plain: Vec<_> = files
        .iter()
        .filter(|file| has(file, "log") || file.as_str() == "samples.jsonl")
        .collect();
    assert!(plain.is_empty(), "plain files left after ending: {plain:?}");
    let compressed: Vec<_> = files.iter().filter(|file| has(file, "gz")).collect();
    assert_eq!(
        compressed,
        [
            "c00/stderr.log.gz",
            "c00/stdout.log.gz",
            "c01/stderr.log.gz",
            "c01/stdout.log.gz",
            "samples.jsonl.gz"
        ]
    );
    assert_eq!(
        retained(fs.as_ref(), trials, "t1"),
        before,
        "older trials are untouched"
    );
    assert_eq!(names(fs.as_ref(), trials), expected);
}

#[test]
fn real_fs_compression_follows_the_directory_it_holds_not_its_pathname() {
    use rustix::fs::{Mode, OFlags, open};
    let dir = crate::testing::tempdir();
    let trials = dir.path().join("trials");
    let trial = trials.join("t1");
    let make = |at: &Path, text: &str| {
        std::fs::create_dir_all(at.join("c00")).unwrap();
        std::fs::write(at.join("samples.jsonl"), text).unwrap();
        std::fs::write(at.join("c00/stdout.log"), text).unwrap();
    };
    make(&trial, "real\n");
    let held = open(
        &trial,
        OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .unwrap();
    // A directory above the trial is renamed and replaced after the descriptor was taken.
    std::fs::rename(&trial, trials.join("moved")).unwrap();
    make(&trial, "decoy\n");

    crate::native::compress_pinned_unsynced(&held).unwrap();

    let moved = trials.join("moved");
    assert!(moved.join("samples.jsonl.gz").exists());
    assert!(!moved.join("samples.jsonl").exists());
    assert!(moved.join("c00/stdout.log.gz").exists());
    assert_eq!(
        std::fs::read_to_string(trial.join("samples.jsonl")).unwrap(),
        "decoy\n"
    );
    assert!(!trial.join("samples.jsonl.gz").exists());
    assert!(!trial.join("c00/stdout.log.gz").exists());
}

#[test]
fn real_fs_runner_samples_are_read_only_from_regular_files_below_the_validated_root() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let dir = crate::testing::tempdir();
    let trials = dir.path().join("trials");
    let options = Options {
        dir: trials.clone(),
        backends: Backends::new().with(Arc::new(TestBackend::new("/bin/true"))),
        topology: topology(),
        identity: None,
        pm_table: None,
    };
    let settings = Settings {
        timing: fast(),
        roots: Roots::default(),
        unscoped: false,
        user_manager: false,
        controller: Arc::new(crate::controller::Recording::default()),
        frequency: super::harness::frequency(&topology()),
        fs: None,
        inputs: crate::procfs::Inputs::default(),
        memory: None,
        clock: Clock::System,
        durability: crate::samples::Durability::Unsynced,
    };
    let runner = Runner::build(options, settings, || {
        Fake::with_memory(crate::watch::Memory::new(Arc::new(MemFs::default())))
    })
    .unwrap();

    assert!(
        runner.samples("t1").next().is_none(),
        "a missing trials root holds no samples"
    );
    let trial = trials.join("t1");
    std::fs::create_dir_all(&trial).unwrap();
    std::fs::set_permissions(&trials, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        runner.samples("t1").next().is_none(),
        "a trial without a samples file holds no samples"
    );

    let samples = trial.join(togi_trialfiles::SAMPLES);
    std::fs::write(&samples, "{\"elapsed_ms\":1}\n").unwrap();
    let read = runner.samples("t1").collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(
        read,
        [togi_machine::TrialConditions {
            elapsed_ms: 1,
            ..Default::default()
        }]
    );

    std::fs::remove_file(&samples).unwrap();
    let victim = dir.path().join("victim.jsonl");
    std::fs::write(&victim, "{\"elapsed_ms\":2}\n").unwrap();
    symlink(&victim, &samples).unwrap();
    let mut read = runner.samples("t1");
    let first = read.next();
    assert!(
        matches!(first, Some(Err(togi_machine::SampleError::Open(_)))),
        "a symlink is never followed: {first:?}"
    );
    assert!(read.next().is_none());
}

/// A sweep whose caller cancels it as it sends SIGTERM to the leftover scope, which is the first
/// cleanup step after discovery. `calls` records the signals and stops; with `stuck`, a scope
/// stop is acknowledged without removing the unit.
fn cancelled_after_term(
    calls: Arc<Mutex<Vec<String>>>,
    cancel: CancellationToken,
    stuck: bool,
) -> impl FnOnce(&Fake) + Send + 'static {
    move |fake| {
        fake.add_unit("togi-trial-old.scope");
        fake.on_systemd(move |call| match call {
            Systemd::Kill { signal, .. } => {
                calls.lock().unwrap().push(signal.name().to_owned());
                if *signal == KillSignal::Term {
                    cancel.cancel();
                }
                None
            }
            Systemd::Stop { .. } => {
                calls.lock().unwrap().push("stop".to_owned());
                stuck.then_some(Reply::Output {
                    success: true,
                    text: String::new(),
                })
            }
            Systemd::ListScopes | Systemd::Probe { .. } => None,
        });
    }
}

#[test]
fn a_sweep_cancelled_after_discovery_still_kills_and_stops_what_it_found() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let cancel = CancellationToken::new();
    let (mut runner, _dir) = runner(
        cancelled_after_term(Arc::clone(&calls), cancel.clone(), false),
        fast(),
    );
    let error = runner.sweep(&cancel).unwrap_err();
    assert!(matches!(error, togi_machine::Error::Cancelled), "{error}");
    let calls = calls.lock().unwrap().clone();
    assert!(calls.contains(&"SIGKILL".to_owned()), "{calls:?}");
    assert!(calls.contains(&"stop".to_owned()), "{calls:?}");
}

#[test]
fn a_sweep_cancelled_after_discovery_reports_the_cleanup_it_could_not_confirm() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let cancel = CancellationToken::new();
    let (mut runner, _dir) = runner(
        cancelled_after_term(Arc::clone(&calls), cancel.clone(), true),
        fast(),
    );
    let error = runner.sweep(&cancel).unwrap_err();
    assert!(
        matches!(error, togi_machine::Error::Containment(_)),
        "an unconfirmed cleanup is not hidden behind the cancellation: {error}"
    );
    assert!(calls.lock().unwrap().contains(&"SIGKILL".to_owned()));
}

#[derive(Clone, Copy)]
enum Phase {
    Reaping,
    Draining,
    Verifying,
}

struct Abandoned {
    rig: super::harness::Rig,
    run: Run,
    member: Rc<Cell<bool>>,
}

/// Stops a trial, abandons its handle while its teardown is in `phase` and lets the cleanup go
/// on; with `fault`, the cleanup meets the failure that phase can meet, which never clears.
async fn abandoned_during(phase: Phase, fault: bool) -> Abandoned {
    let rig = rig();
    let released = Rc::new(Cell::new(false));
    let mut member = Rc::new(Cell::new(false));
    match phase {
        Phase::Reaping => {
            rig.fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.until_killed_ignoring_term().await;
                }))
            });
            if fault {
                rig.fake.on_systemd(|call| {
                    if let Systemd::Kill { .. } = call {
                        Some(Reply::Hang)
                    } else {
                        None
                    }
                });
            }
        }
        Phase::Draining => {
            let released = Rc::clone(&released);
            rig.fake.on_launch(move |_, _| {
                let released = Rc::clone(&released);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.until_killed().await;
                    // The leader is gone, but a descendant still holds the output pipes.
                    while !released.get() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }))
            });
        }
        Phase::Verifying => {
            let survivor = rig.fake.add_recovered(crate::procfs::ScopeProcess {
                scope: ScopeName::from_unit("togi-trial-t1.scope").unwrap(),
                pid: 9000,
                starttime: 1,
            });
            member = survivor.alive;
            rig.fake.on_systemd(|call| {
                if let Systemd::Kill { .. } = call {
                    Some(Reply::Output {
                        success: true,
                        text: String::new(),
                    })
                } else {
                    None
                }
            });
        }
    }
    let handle = rig.start(spec(Regime::R1, &[0], secs(600))).await.unwrap();
    handle.commands.send(TrialCommand::Stop).unwrap();
    let process = rig.fake.process(0);
    match phase {
        // SIGTERM is out and ignored; the grace period runs for 3 s.
        Phase::Reaping => tokio::time::sleep(secs(1)).await,
        Phase::Draining | Phase::Verifying => {
            while !process.reaped.get() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
    if matches!(phase, Phase::Verifying) {
        tokio::time::sleep(secs(1)).await;
    }
    // The `Trial` is dropped here.
    handle.commands.send(TrialCommand::Abandon).unwrap();
    if !fault {
        match phase {
            Phase::Reaping => {}
            Phase::Draining => released.set(true),
            Phase::Verifying => member.set(false),
        }
    }
    let run = finish(&handle).await;
    Abandoned { rig, run, member }
}

crate::testing::test! {
async fn abandoning_while_the_workload_ignores_term_still_kills_and_reaps_it() {
    local(async {
        let clean = abandoned_during(Phase::Reaping, false).await;
        assert!(clean.run.finished.abandoned);
        assert!(clean.run.finished.teardown.is_ok());
        let process = clean.rig.fake.process(0);
        assert!(process.reaped.get());
        assert!(process.signals.borrow().contains(&GroupSignal::Kill));

        let faulty = abandoned_during(Phase::Reaping, true).await;
        assert!(faulty.run.finished.abandoned);
        let containment = faulty.run.finished.teardown.unwrap_err();
        assert!(
            containment
                .failures()
                .iter()
                .any(|failure| matches!(failure, CleanupFailure::KillScope { .. })),
            "no proof that nothing runs while the scope kill is unconfirmed"
        );
        let process = faulty.rig.fake.process(0);
        assert!(process.signals.borrow().contains(&GroupSignal::Kill));
        assert!(process.reaped.get(), "the rest of the cleanup still ran");
    })
    .await;
}
}

crate::testing::test! {
async fn abandoning_while_readers_drain_still_waits_for_them_and_never_proves_an_undrained_stop() {
    local(async {
        let clean = abandoned_during(Phase::Draining, false).await;
        assert!(clean.run.finished.abandoned);
        assert!(clean.run.finished.teardown.is_ok());
        assert!(clean.rig.fake.process(0).reaped.get());

        let faulty = abandoned_during(Phase::Draining, true).await;
        assert!(faulty.run.finished.abandoned);
        let containment = faulty.run.finished.teardown.unwrap_err();
        assert!(
            containment
                .failures()
                .iter()
                .any(|failure| matches!(failure, CleanupFailure::DrainDeadline)),
            "{containment}"
        );
        assert!(faulty.rig.fake.process(0).reaped.get());
    })
    .await;
}
}

crate::testing::test! {
async fn abandoning_during_verification_still_waits_for_the_scope_to_empty() {
    local(async {
        let clean = abandoned_during(Phase::Verifying, false).await;
        assert!(clean.run.finished.abandoned);
        assert!(clean.run.finished.teardown.is_ok());
        assert!(!clean.member.get());

        let faulty = abandoned_during(Phase::Verifying, true).await;
        assert!(faulty.run.finished.abandoned);
        let containment = faulty.run.finished.teardown.unwrap_err();
        assert!(
            matches!(
                containment.failures(),
                [CleanupFailure::ScopeMembers { scope }] if scope == "togi-trial-t1"
            ),
            "{containment}"
        );
        assert!(faulty.member.get(), "the member outlived the teardown");
    })
    .await;
}
}

crate::testing::test! {
async fn shutdown_overlaps_pending_rollback_and_live_teardown_in_one_slot() {
    local(async {
        let timing = Timing {
            stop_grace: Duration::ZERO,
            teardown: secs(8),
            kill_slot: secs(6),
            writer_join: secs(1),
            margin: secs(1),
            ..Timing::default()
        };
        let rig = super::harness::rig_with(TestBackend::new("/bin/true"), timing);
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Hang),
            Systemd::Stop { .. } | Systemd::ListScopes | Systemd::Probe { .. } => None,
        });
        let live = rig.start(spec(Regime::R1, &[0], secs(600))).await.unwrap();
        rig.fake.on_launch(|_, _| {
            Launch::FailRetained(
                script(|mut ctx: ProcCtx| async move { ctx.until_killed().await }),
                std::io::Error::other("retained launch failure"),
            )
        });
        let env = Rc::clone(&rig.env);
        let (reply, rollback) = tokio::sync::oneshot::channel();
        let pending = tokio::task::spawn_local(async move {
            let mut pending = spec(Regime::R1, &[1], secs(600));
            pending.id = "pending".into();
            let result =
                crate::supervisor::start(env, CancellationToken::new(), pending, nothing_runs())
                    .await;
            let _ = reply.send(result.err().unwrap());
        });
        let begun = tokio::time::Instant::now();
        crate::runtime::contain_pending(&rig.env, vec![pending]).await;
        let calls = rig.fake.systemd_calls();
        let kills: Vec<_> = calls
            .iter()
            .filter_map(|(call, at)| match call {
                Systemd::Kill { scope, .. } => Some((scope.as_str(), *at)),
                Systemd::Stop { .. } | Systemd::ListScopes | Systemd::Probe { .. } => None,
            })
            .collect();
        assert_eq!(kills.len(), 2, "{kills:?}");
        assert!(
            kills
                .iter()
                .all(|(_, at)| at.duration_since(begun) < secs(1)),
            "both cleanup commands must start before either slow command finishes: {kills:?}"
        );
        assert!(tokio::time::Instant::now() - begun < timing.stop_bound());
        let rollback = rollback.await.unwrap();
        assert!(rollback.value.is_none());
        let run = tokio::time::timeout(Duration::ZERO, finish(&live))
            .await
            .expect("live teardown completed before shutdown returned");
        assert!(run.finished.abandoned);
        assert!(run.finished.teardown.is_err());
        assert!(rig.fake.process(0).reaped.get());
        assert!(rig.fake.process(1).reaped.get());
    })
    .await;
}
}

crate::testing::test! {
async fn shutdown_retains_and_abandons_a_supervisor_registered_by_a_pending_start() {
    local(async {
        let rig = rig();
        let env = Rc::clone(&rig.env);
        let (reply, answer) = tokio::sync::oneshot::channel();
        let pending = tokio::task::spawn_local(async move {
            tokio::time::sleep(secs(1)).await;
            let handle = crate::supervisor::start(
                env,
                CancellationToken::new(),
                spec(Regime::R1, &[0], secs(600)),
                nothing_runs(),
            )
            .await
            .unwrap();
            let _ = reply.send(handle);
        });
        crate::runtime::contain_pending(&rig.env, vec![pending]).await;
        let handle = answer.await.unwrap();
        let run = tokio::time::timeout(Duration::ZERO, finish(&handle))
            .await
            .expect("late supervisor completed before shutdown returned");
        assert!(run.finished.abandoned);
        assert!(run.finished.teardown.is_ok());
        assert!(rig.fake.process(0).reaped.get());
    })
    .await;
}
}

crate::testing::test! {
async fn a_late_registration_is_abandoned_promptly_while_another_pending_start_stays_blocked() {
    local(async {
        let rig = rig();
        let env = Rc::clone(&rig.env);
        let (reply, answer) = tokio::sync::oneshot::channel();
        let late = tokio::task::spawn_local(async move {
            tokio::time::sleep(secs(1)).await;
            let handle = crate::supervisor::start(
                env,
                CancellationToken::new(),
                spec(Regime::R1, &[0], secs(600)),
                nothing_runs(),
            )
            .await
            .unwrap();
            let _ = reply.send(handle);
        });
        let blocked = tokio::task::spawn_local(std::future::pending::<()>());
        let env = Rc::clone(&rig.env);
        let shutdown = tokio::task::spawn_local(async move {
            crate::runtime::contain_pending(&env, vec![blocked, late]).await;
        });
        let handle = answer.await.unwrap();
        let run = tokio::time::timeout(secs(1), finish(&handle)).await.expect(
            "the late supervisor was abandoned while the blocked start still held shutdown",
        );
        assert!(run.finished.abandoned);
        assert!(!shutdown.is_finished());
        assert!(rig.fake.process(0).reaped.get());
        shutdown.await.unwrap();
    })
    .await;
}
}

crate::testing::test! {
async fn a_handle_dropped_after_the_last_abandon_check_still_reports_an_unconfirmed_teardown() {
    local(async {
        let rig = rig();
        let survivor = rig.fake.add_recovered(crate::procfs::ScopeProcess {
            scope: ScopeName::from_unit("togi-trial-t1.scope").unwrap(),
            pid: 9000,
            starttime: 1,
        });
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Output {
                success: true,
                text: String::new(),
            }),
            Systemd::Stop { .. } | Systemd::ListScopes | Systemd::Probe { .. } => None,
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(600))).await.unwrap();
        let commands = handle.commands.clone();
        let held = Rc::new(std::cell::RefCell::new(Some(handle)));
        crate::supervisor::after_abandon_snapshot({
            let held = Rc::clone(&held);
            move || drop(held.borrow_mut().take())
        });
        crate::supervisor::ABANDON_REPORTS.with_borrow_mut(Vec::clear);
        commands.send(TrialCommand::Stop).unwrap();
        tokio::time::timeout(secs(120), async {
            while held.borrow().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the supervisor reached its publication");
        let reports = crate::supervisor::ABANDON_REPORTS.with_borrow(Clone::clone);
        assert!(survivor.alive.get());
        assert_eq!(
            reports,
            ["togi: trial t1 was dropped while running; teardown unconfirmed: a process remains in scope togi-trial-t1 at the teardown deadline"]
        );
    })
    .await;
}
}

#[derive(Debug)]
struct BoomBackend(TestBackend);

impl Backend for BoomBackend {
    fn configuration(&self, workload: &Workload) -> Result<Vec<u8>, togi_backend::Error> {
        self.0.configuration(workload)
    }

    fn kind(&self) -> togi_machine::Backend {
        self.0.kind()
    }

    fn check(&self) -> Result<Ready, togi_backend::Error> {
        self.0.check()
    }

    fn plan(
        &self,
        workload: &Workload,
        dir: &Path,
        cpus: &[CpuId],
    ) -> Result<Plan, togi_backend::Error> {
        self.0.plan(workload, dir, cpus)
    }

    fn classify(&self, line: &str) -> Line {
        assert!(!line.contains("BOOM"), "the classifier met BOOM");
        self.0.classify(line)
    }
}

struct PanickingReader;

impl PmTableReader for PanickingReader {
    fn pm_table(&self) -> Option<PmTable> {
        panic!("the PM table reader failed");
    }
}

fn panicking_rig() -> super::harness::Rig {
    let backend = TestBackend::new("/bin/true");
    let rig = rig_edit(backend.clone(), Timing::default(), |env| {
        env.pm_table = Some(Arc::new(PanickingReader));
        env.backends = Backends::new().with(Arc::new(BoomBackend(backend)));
    });
    rig.fake.on_launch(|_, _| {
        Launch::Run(script(|mut ctx: ProcCtx| async move {
            while let Some(delivery) = ctx.signals.recv().await {
                if matches!(delivery, Delivery::Group(GroupSignal::Term)) {
                    ctx.print("BOOM\n").await;
                    ctx.exit(ExitInfo::Signal(15));
                    return;
                }
            }
        }))
    });
    rig
}

crate::testing::test! {
async fn a_dropped_handle_reports_both_supervision_panics_in_order_with_the_teardown() {
    local(async {
        let rig = panicking_rig();
        let handle = rig.start(spec(Regime::R1, &[0], secs(600))).await.unwrap();
        crate::supervisor::ABANDON_REPORTS.with_borrow_mut(Vec::clear);
        let commands = handle.commands.clone();
        drop(handle);
        tokio::time::sleep(secs(30)).await;
        assert!(commands.is_closed(), "the supervisor has ended");
        let reports = crate::supervisor::ABANDON_REPORTS.with_borrow(Clone::clone);
        assert_eq!(
            reports,
            ["togi: trial t1 was dropped while running; torn down; \
                 supervision panicked: the PM table reader failed; \
                 supervision panicked: the classifier met BOOM"],
            "{reports:?}"
        );
    })
    .await;
}
}

crate::testing::test! {
async fn a_supervision_panic_keeps_a_classifier_panic_from_the_final_output() {
    local(async {
        let rig = panicking_rig();
        let handle = rig.start(spec(Regime::R1, &[0], secs(600))).await.unwrap();
        let begun = tokio::time::Instant::now();
        let run = tokio::time::timeout(secs(300), finish(&handle))
            .await
            .expect("the supervisor published an end after both panics");
        assert!(begun.elapsed() <= rig.env.timing.stop_bound() + Duration::from_millis(50));
        assert!(run.finished.teardown.is_ok(), "the stop is still proven");
        assert!(rig.fake.process(0).reaped.get());
        assert!(
            matches!(
                &run.finished.errors[..],
                [Error::Panicked { what: first }, Error::Panicked { what: second }]
                    if first.contains("the PM table reader failed")
                        && second.contains("the classifier met BOOM")
            ),
            "{:?}",
            run.finished.errors
        );
        assert!(run.finished.result.inconclusive.is_some());
    })
    .await;
}
}

fn runtime_reports() -> Vec<String> {
    crate::runtime::RUNTIME_REPORTS.with_borrow(Clone::clone)
}

fn undeliverable_start() -> crate::runtime::Command {
    let (reply, nobody) = std::sync::mpsc::sync_channel(1);
    drop(nobody);
    crate::runtime::Command::Start {
        cancel: CancellationToken::new(),
        spec: Box::new(spec(Regime::R1, &[0], secs(600))),
        ready: nothing_runs(),
        reply,
    }
}

crate::testing::test! {
async fn a_start_nobody_waits_for_that_ends_without_proof_is_reported_and_bounded() {
    local(async {
        let rig = rig();
        rig.fake.on_launch(|_, _| {
            Launch::FailRetained(
                script(|mut ctx: ProcCtx| async move { ctx.until_killed().await }),
                std::io::Error::other("retained launch failure"),
            )
        });
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Hang),
            Systemd::Stop { .. } | Systemd::ListScopes | Systemd::Probe { .. } => None,
        });
        crate::runtime::RUNTIME_REPORTS.with_borrow_mut(Vec::clear);
        let begun = tokio::time::Instant::now();
        crate::runtime::serve_detached(Rc::clone(&rig.env), undeliverable_start()).await;
        assert!(begun.elapsed() <= rig.env.timing.stop_bound());
        let reports = runtime_reports();
        assert_eq!(
            reports,
            ["togi: a start nobody waits for ended without confirmed cleanup: trial cleanup could not be confirmed: the start failed: launch instance work in scope togi-trial-t1: retained launch failure; signal scope togi-trial-t1 with SIGKILL: systemctl did not finish within 0ns"]
        );
    })
    .await;
}
}

crate::testing::test! {
async fn a_start_nobody_waits_for_that_rolls_back_with_proof_is_not_reported() {
    local(async {
        let rig = rig();
        rig.fake.on_launch(|_, _| {
            Launch::FailRetained(
                script(|mut ctx: ProcCtx| async move { ctx.until_killed().await }),
                std::io::Error::other("retained launch failure"),
            )
        });
        crate::runtime::RUNTIME_REPORTS.with_borrow_mut(Vec::clear);
        crate::runtime::serve_detached(Rc::clone(&rig.env), undeliverable_start()).await;
        assert!(rig.fake.process(0).reaped.get());
        assert_eq!(runtime_reports(), Vec::<String>::new());
    })
    .await;
}
}

crate::testing::test! {
async fn a_sweep_nobody_waits_for_that_fails_is_reported_and_bounded() {
    local(async {
        let rig = rig();
        rig.fake.add_unit("togi-trial-old.scope");
        rig.fake.on_systemd(|call| match call {
            Systemd::Stop { .. } => Some(Reply::Output {
                success: true,
                text: String::new(),
            }),
            Systemd::Kill { .. } | Systemd::ListScopes | Systemd::Probe { .. } => None,
        });
        let (reply, nobody) = std::sync::mpsc::sync_channel(1);
        drop(nobody);
        crate::runtime::RUNTIME_REPORTS.with_borrow_mut(Vec::clear);
        let begun = tokio::time::Instant::now();
        crate::runtime::serve_detached(
            Rc::clone(&rig.env),
            crate::runtime::Command::Sweep {
                cancel: CancellationToken::new(),
                reply,
            },
        )
        .await;
        assert!(begun.elapsed() <= rig.env.timing.sweep_bound());
        let reports = runtime_reports();
        assert_eq!(
            reports,
            ["togi: a sweep nobody waits for failed: trial cleanup could not be confirmed: leftover trial unit or process remains at the sweep deadline"]
        );
    })
    .await;
}
}

crate::testing::test! {
async fn shutdown_reports_the_live_supervisor_still_running_at_the_shared_deadline() {
    local(async {
        let rig = rig();
        let live = rig.start(spec(Regime::R1, &[0], secs(600))).await.unwrap();
        let (commands, mut inbox) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::task::spawn_local(async move {
            let _inbox = inbox.recv().await;
            std::future::pending::<()>().await;
        });
        crate::supervisor::register(
            &rig.env,
            crate::supervisor::Supervised {
                trial: "wedged".into(),
                commands,
                task,
            },
        );
        crate::runtime::RUNTIME_REPORTS.with_borrow_mut(Vec::clear);
        let begun = tokio::time::Instant::now();
        crate::runtime::contain_pending(&rig.env, Vec::new()).await;
        assert_eq!(begun.elapsed(), rig.env.timing.stop_bound());
        assert_eq!(
            runtime_reports(),
            ["togi: trial wedged was still not torn down when its runner shut down"]
        );
        let run = tokio::time::timeout(Duration::ZERO, finish(&live))
            .await
            .expect("the other live supervisor was torn down inside the slot");
        assert!(run.finished.abandoned);
        assert!(run.finished.teardown.is_ok());
        assert!(rig.fake.process(0).reaped.get());
    })
    .await;
}
}

#[test]
fn a_cached_cancellation_joined_to_a_failed_teardown_stays_containment_with_every_diagnostic() {
    let (mut runner, _dir) = live_runner(
        |fake| {
            fake.on_systemd(|call| {
                if let Systemd::Kill { .. } = call {
                    Some(Reply::Hang)
                } else {
                    None
                }
            });
        },
        fast(),
    );
    let mut trial = start(&mut runner, Regime::R1, &[0], secs(600));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut seen = Vec::new();
    for round in 0..2 {
        let partial = trial.wait(&cancel, &mut Collect::default()).unwrap_err();
        let togi_machine::Error::Containment(inner) = partial.error else {
            panic!("round {round}: the machine category is not containment: {partial:?}");
        };
        let Some(Error::Joined(joined)) = inner
            .source()
            .and_then(|source| source.downcast_ref::<Error>())
        else {
            panic!("round {round}: no joined diagnostics behind {inner}");
        };
        let [Error::Containment(containment), Error::Cancelled] = joined.errors() else {
            panic!("round {round}: {joined}");
        };
        assert!(matches!(
            containment.failures().first(),
            Some(CleanupFailure::KillScope { .. })
        ));
        seen.push(joined.to_string());
    }
    assert_eq!(seen[0], seen[1]);
    assert!(matches!(
        trial.stop(),
        Err(togi_machine::Error::Containment(_))
    ));
}

#[test]
fn a_containment_shows_each_failure_with_its_causes_once_in_order() {
    let mut containment = crate::Containment::new(CleanupFailure::Start(Box::new(Error::Launch {
        instance: "work".into(),
        scope: "togi-trial-t1".into(),
        source: std::io::Error::other("no such file"),
    })));
    containment.push(CleanupFailure::KillScope {
        scope: "togi-trial-t1".into(),
        signal: "SIGKILL",
        source: crate::SystemdError::Command(togi_command::Error::TimedOut {
            program: "systemctl".into(),
            timeout: std::time::Duration::from_secs(5),
        }),
    });
    assert_eq!(
        containment.to_string(),
        "the start failed: launch instance work in scope togi-trial-t1: no such file; \
         signal scope togi-trial-t1 with SIGKILL: systemctl did not finish within 5s"
    );
}

crate::testing::test! {
async fn a_cancelled_sweep_nobody_waits_for_with_failed_cleanup_reports_every_cause_in_order() {
    local(async {
        let rig = rig();
        let cancel = CancellationToken::new();
        rig.fake.add_unit("togi-trial-old.scope");
        rig.fake.on_systemd({
            let cancel = cancel.clone();
            move |call| match call {
                Systemd::Kill { signal, .. } => {
                    if *signal == KillSignal::Term {
                        cancel.cancel();
                    }
                    None
                }
                Systemd::Stop { .. } => Some(Reply::Output {
                    success: false,
                    text: "unit busy".into(),
                }),
                Systemd::ListScopes | Systemd::Probe { .. } => None,
            }
        });
        let (reply, nobody) = std::sync::mpsc::sync_channel(1);
        drop(nobody);
        crate::runtime::RUNTIME_REPORTS.with_borrow_mut(Vec::clear);
        crate::runtime::serve_detached(
            Rc::clone(&rig.env),
            crate::runtime::Command::Sweep { cancel, reply },
        )
        .await;
        assert_eq!(
            runtime_reports(),
            ["togi: a sweep nobody waits for failed: cancelled; trial cleanup could not be confirmed: \
              stop scope togi-trial-old: exited with exit status: 1: unit busy; \
              leftover trial unit or process remains at the sweep deadline"]
        );
    })
    .await;
}
}

#[test]
fn stopping_without_waiting_has_terminated_the_workload_before_it_returns() {
    let terminated = Arc::new(AtomicBool::new(false));
    let (mut runner, _keep) = live_runner(marks_termination(Arc::clone(&terminated)), fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    assert!(trial.stop().is_ok());
    assert!(
        terminated.load(Ordering::SeqCst),
        "stop returned before the workload ended"
    );
}

/// SHA-256 of `abc`, from FIPS 180-2, so the expectation does not come from the code under test.
const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

fn mprime_workload() -> togi_machine::WorkloadId {
    togi_machine::WorkloadId::by_id("mprime-sse-4k-21k").unwrap()
}

fn sha256_of(bytes: &[u8]) -> togi_machine::Digest {
    use sha2::Digest as _;
    togi_machine::Digest::from_bytes(sha2::Sha256::digest(bytes).into())
}

#[test]
fn content_is_the_hash_of_the_executable_and_of_the_configuration() {
    let (mut runner, fs) = runner(|_| {}, fast());
    fs.put("/bin/true", "abc");
    let content = runner.content(mprime_workload()).unwrap();
    assert_eq!(content.backend, togi_machine::Backend::Mprime);
    assert_eq!(content.executable.as_str(), ABC_SHA256);
    assert_eq!(
        content.configuration,
        sha256_of(&togi_backend::configuration_bytes(&[], &[]))
    );
}

#[test]
fn content_reads_each_executable_once_for_the_runners_life() {
    let (mut runner, fs) = runner(|_| {}, fast());
    fs.put("/bin/true", "abc");
    fs.record_history();
    let first = runner.content(mprime_workload()).unwrap();
    fs.put("/bin/true", "replaced after the first read");
    let again = runner.content(mprime_workload()).unwrap();
    let other = togi_machine::WorkloadId::by_id("mprime-avx2-36k-248k").unwrap();
    let elsewhere = runner.content(other).unwrap();
    assert_eq!(first, again);
    assert_eq!(first.executable, elsewhere.executable);
    let reads = fs
        .reads()
        .into_iter()
        .filter(|path| path == Path::new("/bin/true"))
        .count();
    assert_eq!(reads, 1);
}

#[test]
fn content_of_an_unreadable_executable_is_an_error_and_is_retried() {
    let (mut runner, fs) = runner(|_| {}, fast());
    let missing = runner.content(mprime_workload()).unwrap_err();
    assert!(
        togi_machine::error_chain(&missing).contains("read the backend executable /bin/true"),
        "{missing}"
    );
    fs.put("/bin/true", "abc");
    assert_eq!(
        runner
            .content(mprime_workload())
            .unwrap()
            .executable
            .as_str(),
        ABC_SHA256
    );
}

#[test]
fn content_of_a_backend_that_is_not_configured_is_backend_missing() {
    let (mut runner, _fs) = runner(|_| {}, fast());
    let ycruncher = togi_machine::WorkloadId::by_id("ycruncher-bkt-sftv4").unwrap();
    assert!(matches!(
        runner.content(ycruncher),
        Err(togi_machine::Error::BackendMissing)
    ));
}

#[test]
fn content_is_refused_inside_an_async_runtime() {
    let (mut runner, fs) = runner(|_| {}, fast());
    fs.put("/bin/true", "abc");
    let refused = crate::testing::runtime(true)
        .block_on(async { runner.content(mprime_workload()) })
        .unwrap_err();
    assert!(
        togi_machine::error_chain(&refused).contains("content blocks"),
        "{refused}"
    );
}

#[test]
fn real_fs_content_hashes_the_executable_file() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("stress");
    std::fs::write(&program, "abc").unwrap();
    let options = Options {
        dir: dir.path().join("trials"),
        backends: Backends::new().with(Arc::new(TestBackend::new(&program))),
        topology: topology(),
        identity: None,
        pm_table: None,
    };
    let mut runner = Runner::new(options).unwrap();
    let content = runner.content(mprime_workload()).unwrap();
    assert_eq!(content.executable.as_str(), ABC_SHA256);
    std::fs::write(&program, "changed on disk").unwrap();
    assert_eq!(runner.content(mprime_workload()).unwrap(), content);
}
