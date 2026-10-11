//! The public seams end to end: the runtime thread, the sync API and abandoned handles. These run
//! in real time on the fake OS, with short timings where a bound is under test.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use togi_backend::{Backend, Line, Plan, Ready};
use togi_machine::{
    CancellationToken, CoreId, CpuId, Partial, PmTable, PmTableReader, Regime, Reporter, Running,
    Sample, Signal, Stopped, Trials, Workload,
};

use super::harness::{
    Run, crate_error, finish, local, nothing_runs, own_identity, rig, rig_edit, spec, topology,
};
use crate::error::{CleanupFailure, Error, InvalidTrial};
use crate::id::ScopeName;
use crate::native::Settings;
use crate::os::fake::{Delivery, Fake, Launch, ProcCtx, Reply, script};
use crate::os::{ExitInfo, GroupSignal, KillSignal, Systemd};
use crate::runner::{Backends, Options, Runner};
use crate::sensors::Roots;
use crate::supervisor::TrialCommand;
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

fn runner(setup: impl FnOnce(&Fake) + Send + 'static, timing: Timing) -> (Runner, TempDir) {
    let dir = crate::testing::tempdir();
    let options = Options {
        dir: dir.path().join("trials"),
        backends: Backends::new().with(Arc::new(TestBackend::new("/bin/true"))),
        topology: topology(),
        identity: Some(own_identity()),
        pm_table: None,
    };
    let roots = Roots {
        hwmon: dir.path().join("hwmon"),
        cpu: dir.path().join("cpu"),
        powercap: dir.path().join("powercap"),
        proc: dir.path().join("proc"),
    };
    let settings = Settings {
        timing,
        roots,
        unscoped: false,
        user_manager: false,
    };
    let runner = Runner::build(options, settings, move || {
        let fake = Fake::default();
        setup(&fake);
        fake
    })
    .unwrap();
    (runner, dir)
}

#[derive(Default)]
struct Collect {
    progress: Vec<String>,
    signals: Vec<(CoreId, Signal)>,
}

impl Reporter for Collect {
    fn progress(&mut self, detail: &str) {
        self.progress.push(detail.to_owned());
    }

    fn sample(&mut self, _: Sample) {}

    fn signal(&mut self, core: CoreId, signal: Signal, _: &str) {
        self.signals.push((core, signal));
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

/// A worker that notes in `marker` when it is asked to terminate, then ends.
fn terminating(marker: &Path) -> Launch {
    let marker = marker.to_owned();
    Launch::Run(script(move |mut ctx: ProcCtx| async move {
        while let Some(delivery) = ctx.signals.recv().await {
            if matches!(delivery, Delivery::Group(GroupSignal::Term)) {
                std::fs::write(&marker, "terminated").unwrap();
                ctx.exit(ExitInfo::Signal(15));
                return;
            }
        }
    }))
}

fn marks_termination(marker: PathBuf) -> impl FnOnce(&Fake) + Send + 'static {
    move |fake| {
        fake.on_launch(move |_, _| terminating(&marker));
    }
}

/// Timings whose start bound a blocked launch can outlast: one instance may take 850 ms.
fn stalling() -> Timing {
    Timing {
        sample: Duration::from_millis(50),
        stall_grace: Duration::from_millis(150),
        stop_grace: Duration::from_millis(50),
        teardown: Duration::from_millis(400),
        kill_slot: Duration::from_millis(100),
        drain: Duration::from_millis(200),
        writer_join: Duration::from_millis(300),
        prepare: Duration::from_millis(200),
        margin: Duration::from_millis(100),
        ..Timing::default()
    }
}

/// Workers that note termination in `marker`, the first launch of which blocks the runtime thread
/// for `stall` first, as slow filesystem or launcher setup does. `launches` counts the launches;
/// `ready` is whether the workers start inside their scope.
fn stalls_first_launch(
    stall: Duration,
    marker: PathBuf,
    launches: Arc<AtomicUsize>,
    ready: bool,
) -> impl FnOnce(&Fake) + Send + 'static {
    move |fake| {
        fake.set_ready_default(ready);
        fake.on_launch(move |_, _| {
            if launches.fetch_add(1, Ordering::SeqCst) == 0 {
                std::thread::sleep(stall);
            }
            terminating(&marker)
        });
    }
}

#[test]
fn a_whole_trial_runs_waits_and_stops_through_the_public_seams() {
    let (mut runner, dir) = runner(
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
    assert!(dir.path().join("trials/t1/work/stdout.log").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("trials/t1/work/stdout.log")).unwrap(),
        "ok one\nok two\n"
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
    assert_eq!(
        result.failure.map(|f| f.signal),
        Some(Signal::ComputationError)
    );
    assert!(trial.stop().is_ok());
}

#[test]
fn stopping_without_waiting_tears_down_and_proves_it() {
    let (mut runner, _dir) = runner(|_| {}, fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    assert!(trial.stop().is_ok());
}

#[test]
fn readers_keep_draining_between_start_and_wait() {
    let (mut runner, dir) = runner(
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
    let log = dir.path().join("trials/t1/work/stdout.log");
    let expected = 200 * 100 * "ok line of output\n".len() as u64;
    assert!(
        wait_for(secs(10), || std::fs::metadata(&log)
            .is_ok_and(|m| m.len() == expected)),
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

#[tokio::test]
async fn the_blocking_apis_refuse_to_run_inside_a_runtime() {
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
}

#[test]
fn waiting_and_stopping_inside_a_runtime_are_refused_and_the_trial_stays_usable() {
    let (mut runner, _dir) = runner(|_| {}, fast());
    let mut trial = start(&mut runner, Regime::R1, &[0], Duration::from_millis(200));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
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
    let dir = crate::testing::tempdir();
    let marker = dir.path().join("terminated.txt");
    let (mut runner, _keep) = runner(marks_termination(marker.clone()), fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    drop(trial);
    assert!(
        wait_for(secs(10), || marker.exists()),
        "SIGTERM reached the abandoned workload"
    );
}

#[test]
fn a_trial_dropped_while_supervision_runs_is_torn_down_too() {
    let dir = crate::testing::tempdir();
    let marker = dir.path().join("terminated.txt");
    let (mut runner, _keep) = runner(marks_termination(marker.clone()), fast());
    let trial = start(&mut runner, Regime::R7, &[0, 1], secs(600));
    std::thread::sleep(Duration::from_millis(200));
    drop(trial);
    assert!(wait_for(secs(10), || marker.exists()));
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

#[test]
fn cancelling_a_wait_returns_a_partial_result_and_stop_still_proves_the_teardown() {
    let (mut runner, _dir) = runner(|_| {}, fast());
    let mut trial = start(&mut runner, Regime::R1, &[0], secs(600));
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        trigger.cancel();
    });
    let partial = trial.wait(&cancel, &mut Collect::default()).unwrap_err();
    canceller.join().unwrap();
    assert!(matches!(partial.error, togi_machine::Error::Cancelled));
    assert!(partial.value.ran < secs(5));
    assert!(trial.stop().is_ok());
}

#[test]
fn an_unconfirmed_teardown_is_containment_for_wait_and_for_stop() {
    let (mut runner, _dir) = runner(
        |fake| {
            fake.on_systemd(|call| match call {
                Systemd::Kill { .. } => Some(Reply::Hang),
                _ => None,
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
            fake.on_systemd(|call| match call {
                Systemd::Kill { .. } => Some(Reply::Hang),
                _ => None,
            });
        },
        fast(),
    );
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    std::mem::forget(trial);
    let begun = Instant::now();
    drop(runner);
    let bound = fast().stop_bound() + secs(1);
    assert!(begun.elapsed() < bound + secs(1), "{:?}", begun.elapsed());
}

#[test]
fn dropping_the_runner_tears_down_a_trial_whose_handle_was_never_dropped() {
    let dir = crate::testing::tempdir();
    let marker = dir.path().join("terminated.txt");
    let (mut runner, _keep) = runner(marks_termination(marker.clone()), fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    std::mem::forget(trial);
    drop(runner);
    assert!(
        marker.exists(),
        "the supervisor terminated its workload before the runner ended"
    );
}

#[test]
fn the_runner_sweeps_probes_compresses_and_reads_samples() {
    let (mut runner, dir) = runner(
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
    assert!(dir.path().join("trials/t1/work/stdout.log.gz").exists());
    assert!(!dir.path().join("trials/t1/work/stdout.log").exists());
    assert!(runner.samples("t1").count() >= 1);
    assert_eq!(runner.samples("../t1").count(), 0);
    assert!(runner.ended("../t1").is_err());
}

#[test]
fn a_cancelled_sweep_returns_cancelled_without_waiting_out_its_deadline() {
    let (mut runner, _dir) = runner(
        |fake| {
            fake.on_systemd(|call| match call {
                Systemd::ListScopes => Some(Reply::Hang),
                _ => None,
            });
        },
        Timing::default(),
    );
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        trigger.cancel();
    });
    let begun = Instant::now();
    let error = runner.sweep(&cancel).unwrap_err();
    assert!(matches!(error, togi_machine::Error::Cancelled));
    assert!(begun.elapsed() < secs(5));
}

#[test]
fn a_trial_longer_than_the_teardown_budgets_still_completes_through_wait() {
    let timing = fast();
    let (mut runner, _dir) = runner(|_| {}, timing);
    let long = timing.teardown + timing.writer_join + timing.margin + Duration::from_secs(1);
    let mut trial = start(&mut runner, Regime::R1, &[0], long);
    let begun = Instant::now();
    let result = trial
        .wait(&CancellationToken::new(), &mut Collect::default())
        .map_err(|partial| partial.error)
        .unwrap();
    assert_eq!(result.ran, long);
    assert!(begun.elapsed() + Duration::from_millis(100) >= long);
    assert!(trial.stop().is_ok());
}

fn silent_supervisor(
    duration: Duration,
    timing: &Timing,
) -> (
    crate::native::Trial,
    std::sync::mpsc::Sender<crate::supervisor::Event>,
) {
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
    (crate::native::Trial::new(handle, timing), events_tx)
}

#[test]
fn wait_gives_up_on_a_supervisor_that_never_answers_at_its_bound() {
    let timing = Timing {
        teardown: Duration::from_millis(100),
        writer_join: Duration::from_millis(100),
        margin: Duration::from_millis(100),
        ..Timing::default()
    };
    let (mut trial, _kept_open) = silent_supervisor(Duration::from_millis(200), &timing);
    let begun = Instant::now();
    let partial = trial
        .wait(&CancellationToken::new(), &mut Collect::default())
        .unwrap_err();
    let elapsed = begun.elapsed();
    assert!(
        elapsed >= Duration::from_millis(450) && elapsed < secs(3),
        "{elapsed:?}"
    );
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
    assert_eq!(partial.value, togi_machine::TrialResult::default());
}

#[test]
fn stop_gives_up_on_a_supervisor_that_never_answers_with_containment() {
    let timing = Timing {
        teardown: Duration::from_millis(100),
        writer_join: Duration::from_millis(100),
        margin: Duration::from_millis(100),
        ..Timing::default()
    };
    let (trial, _kept_open) = silent_supervisor(secs(60), &timing);
    let begun = Instant::now();
    let outcome = Box::new(trial).stop();
    assert!(matches!(outcome, Err(togi_machine::Error::Containment(_))));
    assert!(begun.elapsed() >= Duration::from_millis(250) && begun.elapsed() < secs(3));
}

#[test]
fn the_wait_bound_covers_the_whole_duration_of_a_long_trial() {
    let timing = Timing::default();
    let (trial, _kept_open) = silent_supervisor(secs(900), &timing);
    let remaining = trial.wait_budget();
    assert!(remaining > secs(900) + timing.teardown + timing.writer_join);
}

struct Slow;

impl Reporter for Slow {
    fn progress(&mut self, _: &str) {
        std::thread::sleep(Duration::from_millis(10));
    }

    fn sample(&mut self, _: Sample) {}

    fn signal(&mut self, _: CoreId, _: Signal, _: &str) {}
}

#[test]
fn a_backlog_of_reports_cannot_carry_wait_past_its_absolute_bound() {
    let timing = Timing {
        teardown: Duration::from_millis(100),
        writer_join: Duration::from_millis(100),
        margin: Duration::from_millis(100),
        ..Timing::default()
    };
    let (mut trial, events) = silent_supervisor(Duration::from_millis(200), &timing);
    for _ in 0..1000 {
        events
            .send(crate::supervisor::Event::Progress("ok".into()))
            .unwrap();
    }
    let begun = Instant::now();
    let outcome = trial.wait(&CancellationToken::new(), &mut Slow);
    assert!(outcome.is_err());
    assert!(begun.elapsed() < secs(2), "{:?}", begun.elapsed());
}

#[test]
fn containment_carries_the_ordered_cleanup_failures_as_its_source() {
    let (mut runner, _dir) = runner(
        |fake| {
            fake.on_systemd(|call| match call {
                Systemd::Kill { .. } => Some(Reply::Hang),
                _ => None,
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
    let dir = crate::testing::tempdir();
    let marker = dir.path().join("terminated.txt");
    let (mut runner, _keep) = runner(
        stalls_first_launch(secs(1), marker.clone(), Arc::new(AtomicUsize::new(0)), true),
        stalling(),
    );
    let begun = Instant::now();
    let partial = start_failure(&mut runner, Regime::R1, &[0], secs(600));
    assert!(begun.elapsed() < secs(5), "{:?}", begun.elapsed());
    assert!(partial.value.is_none(), "no proof that nothing runs");
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
    assert!(
        wait_for(secs(10), || marker.exists()),
        "the workload of a start nobody waits for was torn down"
    );
}

#[test]
fn a_timed_out_start_launches_no_further_instance_and_rolls_back_the_first() {
    let dir = crate::testing::tempdir();
    let marker = dir.path().join("terminated.txt");
    let launches = Arc::new(AtomicUsize::new(0));
    let (mut runner, _keep) = runner(
        stalls_first_launch(
            Duration::from_millis(1200),
            marker.clone(),
            Arc::clone(&launches),
            true,
        ),
        stalling(),
    );
    let partial = start_failure(&mut runner, Regime::R7, &[0, 1], secs(600));
    assert!(partial.value.is_none());
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
    assert!(
        wait_for(secs(10), || marker.exists()),
        "the first instance was rolled back"
    );
    assert_eq!(
        launches.load(Ordering::SeqCst),
        1,
        "a start nobody waits for launches nothing more"
    );
}

#[test]
fn dropping_the_runner_after_a_timed_out_start_waits_for_the_rollback_in_progress() {
    let dir = crate::testing::tempdir();
    let marker = dir.path().join("terminated.txt");
    let (mut runner, _keep) = runner(
        stalls_first_launch(
            secs(1),
            marker.clone(),
            Arc::new(AtomicUsize::new(0)),
            false,
        ),
        stalling(),
    );
    let partial = start_failure(&mut runner, Regime::R6, &[0], secs(600));
    assert!(partial.value.is_none());
    drop(runner);
    assert!(
        marker.exists(),
        "shutdown let the start that was waiting for its scope roll its workload back"
    );
}

#[test]
fn dropping_the_trial_and_the_runner_back_to_back_still_terminates_the_workload() {
    let dir = crate::testing::tempdir();
    let marker = dir.path().join("terminated.txt");
    let (mut runner, _keep) = runner(marks_termination(marker.clone()), fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    drop(trial);
    drop(runner);
    assert!(
        marker.exists(),
        "the runner ended only after the abandoned workload was terminated"
    );
}

#[test]
fn a_duration_that_cannot_be_scheduled_is_refused_before_anything_changes() {
    let (mut runner, dir) = runner(|_| {}, fast());
    let partial = start_failure(&mut runner, Regime::R1, &[0], Duration::MAX);
    assert!(partial.value.is_some(), "nothing ran, so the proof returns");
    assert!(matches!(
        crate_error(&partial.error),
        Some(Error::Invalid(InvalidTrial::UnrepresentableDuration))
    ));
    assert!(!dir.path().join("trials").exists());
}

#[test]
fn a_repeated_wait_reports_the_failure_the_first_one_did() {
    let (mut runner, _dir) = runner(|_| {}, fast());
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

#[test]
fn a_cancelled_trial_keeps_the_output_failure_that_followed_its_cancellation() {
    let (mut runner, _dir) = runner(
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
    let (mut runner, _dir) = runner(|_| {}, quick());
    let mut trial = start(&mut runner, Regime::R1, &[0], Duration::from_millis(100));
    std::thread::sleep(Duration::from_millis(1300));
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
    assert!(trial.stop().is_ok());
}

#[test]
fn a_late_wait_with_a_backlog_of_reports_is_still_bounded() {
    let (mut runner, _dir) = runner(
        |fake| {
            fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.print(&"ok line\n".repeat(300)).await;
                    ctx.until_killed().await;
                }))
            });
        },
        quick(),
    );
    let mut trial = start(&mut runner, Regime::R1, &[0], Duration::from_millis(100));
    std::thread::sleep(Duration::from_millis(1300));
    let begun = Instant::now();
    let partial = trial
        .wait(&CancellationToken::new(), &mut Slow)
        .unwrap_err();
    assert!(begun.elapsed() < secs(2), "{:?}", begun.elapsed());
    assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Every file of every trial directory except `skip`, by path.
fn retained(root: &Path, skip: &str) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    for trial in std::fs::read_dir(root).unwrap() {
        let trial = trial.unwrap();
        if trial.file_name() == skip {
            continue;
        }
        for file in std::fs::read_dir(trial.path()).unwrap() {
            let path = file.unwrap().path();
            found.insert(path.clone(), std::fs::read(path).unwrap());
        }
    }
    found
}

fn files_below(dir: &Path, prefix: &str, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = format!("{prefix}{}", entry.file_name().to_str().unwrap());
        if entry.file_type().unwrap().is_dir() {
            files_below(&entry.path(), &format!("{name}/"), found);
        } else {
            found.push(name);
        }
    }
    found.sort();
}

#[test]
fn ending_a_trial_compresses_it_and_leaves_every_older_retained_trial_untouched() {
    let (mut runner, dir) = runner(|_| {}, fast());
    let trials = dir.path().join("trials");
    for i in 0..205 {
        let old = trials.join(format!("{:04}", 9000 + i));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("passed"), "").unwrap();
        std::fs::write(old.join("samples.jsonl.gz"), "old").unwrap();
    }
    let mut expected = names(&trials);
    expected.push("t1".to_owned());
    expected.sort();
    let before = retained(&trials, "t1");
    assert_eq!(before.len(), 410);

    let mut trial = start(&mut runner, Regime::R7, &[0, 1], Duration::from_millis(300));
    let _ = trial.wait(&CancellationToken::new(), &mut Collect::default());
    drop(trial.stop().unwrap());
    runner.ended("t1").unwrap();

    let mut files = Vec::new();
    files_below(&trials.join("t1"), "", &mut files);
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
        retained(&trials, "t1"),
        before,
        "older trials are untouched"
    );
    assert_eq!(names(&trials), expected);
}

#[test]
fn compression_follows_the_directory_it_holds_not_its_pathname() {
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

    crate::native::compress_pinned(&held).unwrap();

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
            _ => None,
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
                rig.fake.on_systemd(|call| match call {
                    Systemd::Kill { .. } => Some(Reply::Hang),
                    _ => None,
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
            rig.fake.on_systemd(|call| match call {
                Systemd::Kill { .. } => Some(Reply::Output {
                    success: true,
                    text: String::new(),
                }),
                _ => None,
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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
            _ => None,
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
                _ => None,
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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
            _ => None,
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

#[derive(Debug)]
struct BoomBackend(TestBackend);

impl Backend for BoomBackend {
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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
            _ => None,
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
async fn a_sweep_nobody_waits_for_that_fails_is_reported_and_bounded() {
    local(async {
        let rig = rig();
        rig.fake.add_unit("togi-trial-old.scope");
        rig.fake.on_systemd(|call| match call {
            Systemd::Stop { .. } => Some(Reply::Output {
                success: true,
                text: String::new(),
            }),
            _ => None,
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

#[tokio::test(start_paused = true)]
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

#[test]
fn a_cached_cancellation_joined_to_a_failed_teardown_stays_containment_with_every_diagnostic() {
    let (mut runner, _dir) = runner(
        |fake| {
            fake.on_systemd(|call| match call {
                Systemd::Kill { .. } => Some(Reply::Hang),
                _ => None,
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
