//! The public seams end to end: the runtime thread, the sync API and abandoned handles. These run
//! in real time on the fake OS, with short timings where a bound is under test.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use togi_machine::{
    CancellationToken, CoreId, Partial, Regime, Reporter, Running, Sample, Signal, Stopped, Trials,
};

use super::harness::{crate_error, nothing_runs, own_identity, spec, topology};
use crate::error::Error;
use crate::os::fake::{Delivery, Fake, Launch, ProcCtx, Reply, script};
use crate::os::{ExitInfo, GroupSignal, Systemd};
use crate::runner::{Backends, Options, Runner, Settings};
use crate::sensors::Roots;
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
        ..Timing::default()
    }
}

fn runner(
    setup: impl FnOnce(&Fake) + Send + 'static,
    timing: Timing,
) -> (Runner, TempDir) {
    let dir = tempfile::tempdir().unwrap();
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

fn start(runner: &mut Runner, regime: Regime, cores: &[u8], duration: Duration) -> Box<dyn Running> {
    runner
        .start(&CancellationToken::new(), spec(regime, cores, duration), nothing_runs())
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
fn marks_termination(
    marker: std::path::PathBuf,
) -> impl FnOnce(&Fake) + Send + 'static {
    move |fake| {
        fake.on_launch(move |_, _| {
            let marker = marker.clone();
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                while let Some(delivery) = ctx.signals.recv().await {
                    if matches!(delivery, Delivery::Group(GroupSignal::Term)) {
                        std::fs::write(&marker, "terminated").unwrap();
                        ctx.exit(ExitInfo::Signal(15));
                        return;
                    }
                }
            }))
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
    assert_eq!(result.failure.map(|f| f.signal), Some(Signal::ComputationError));
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
        wait_for(secs(10), || std::fs::metadata(&log).is_ok_and(|m| m.len() == expected)),
        "the log filled before anyone waited"
    );
    let mut report = Collect::default();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let outcome = trial.wait(&cancel, &mut report);
    assert!(matches!(outcome, Err(Partial { error: togi_machine::Error::Cancelled, .. })));
    assert_eq!(report.progress.len(), 200 * 100, "every queued progress line reaches the waiter");
    assert!(trial.stop().is_ok());
}

#[tokio::test]
async fn the_blocking_apis_refuse_to_run_inside_a_runtime() {
    let (mut runner, _dir) = runner(|_| {}, fast());
    assert!(matches!(
        runner.probe_scope(),
        Err(Error::InsideRuntime { operation: "the scope probe" })
    ));
    let sweep = runner.sweep(&CancellationToken::new()).unwrap_err();
    assert!(matches!(
        crate_error(&sweep),
        Some(Error::InsideRuntime { operation: "the sweep" })
    ));
    let partial = runner
        .start(&CancellationToken::new(), spec(Regime::R1, &[0], secs(1)), nothing_runs())
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
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("terminated.txt");
    let (mut runner, _keep) = runner(marks_termination(marker.clone()), fast());
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    drop(trial);
    assert!(wait_for(secs(10), || marker.exists()), "SIGTERM reached the abandoned workload");
}

#[test]
fn a_trial_dropped_while_supervision_runs_is_torn_down_too() {
    let dir = tempfile::tempdir().unwrap();
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
    let partial = trial.wait(&CancellationToken::new(), &mut Collect::default()).unwrap_err();
    assert!(matches!(partial.error, togi_machine::Error::Containment));
    assert_eq!(partial.value.failure, None, "evidence is kept beside the containment");
    assert!(matches!(trial.stop(), Err(togi_machine::Error::Containment)));
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
        Timing::default(),
    );
    let trial = start(&mut runner, Regime::R1, &[0], secs(600));
    std::mem::forget(trial);
    let begun = Instant::now();
    drop(runner);
    assert!(begun.elapsed() < secs(3), "{:?}", begun.elapsed());
}

#[test]
fn the_runner_sweeps_probes_compresses_and_reads_samples() {
    let (mut runner, dir) = runner(
        |fake| {
            fake.add_unit("togi-trial-old.scope");
        },
        fast(),
    );
    assert_eq!(runner.probe_scope().unwrap(), "scope confined to cpu 0 created");
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
