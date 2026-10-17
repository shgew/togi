use std::cell::Cell;
use std::io;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use test_case::test_case;
use togi_backend::{Backend, Line, Plan, Ready};
use togi_machine::{
    CancellationToken, CpuId, LoadSchedule, Partial, PmTable, PmTableReader, Regime, Signal,
    Stopped, TrialSpec, Workload,
};
use tokio::time::Instant;

use super::harness::{
    Rig, burn, core, crate_error, finish, local, nothing_runs, printing, rig, rig_edit, rig_with,
    spec,
};
use crate::error::{Error, InvalidTrial, PrepareError};
use crate::files::Chown;
use crate::id::ScopeName;
use crate::logs::{Sink, Wrap};
use crate::os::fake::{Fake, Launch, ProcCtx, ProcShared, lost_read, script};
use crate::os::{ExitInfo, GroupSignal, LaunchFailure, Os, Systemd};
use crate::procfs::{ProcError, ScopeProcess, Thread, Threads, Usage};
use crate::runner::Backends;
use crate::samples::{Open, Storage};
use crate::supervisor::{Env, Handle, TrialCommand, start};
use crate::testing::TestBackend;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn a_quiet_trial_runs_its_whole_duration_and_teardown_stops_it() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[2], secs(10))).await.unwrap();
            let started = tokio::time::Instant::now();
            let run = finish(&handle).await;
            assert!(started.elapsed() >= secs(10));
            assert!(run.finished.teardown.is_ok());
            assert!(run.finished.errors.is_empty());
            let result = &run.finished.result;
            assert_eq!(
                (result.failure, result.inconclusive.as_deref()),
                (None, None)
            );
            assert_eq!(result.ran, secs(10));
            let process = rig.fake.process(0);
            assert!(process.reaped.get());
            assert_eq!(
                process.signals.borrow()[..],
                [GroupSignal::Cont, GroupSignal::Term, GroupSignal::Kill]
            );
        })
        .await;
    });
}

#[test]
fn progress_comes_from_stdout_and_stderr_progress_is_ignored() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.print("ok step one\nplain\n").await;
                    ctx.eprint("ok from stderr\n").await;
                    ctx.until_killed().await;
                }))
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(5))).await.unwrap();
            let run = finish(&handle).await;
            assert_eq!(run.progress(), ["ok step one"]);
        })
        .await;
    });
}

#[test]
fn a_computation_error_ends_the_trial_and_names_the_core() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(printing("COMPUTE ERROR\n"));
            let handle = rig.start(spec(Regime::R1, &[3], secs(60))).await.unwrap();
            let run = finish(&handle).await;
            let failure = run.finished.result.failure.unwrap();
            assert_eq!(
                (failure.signal, failure.core),
                (Signal::ComputationError, core(3))
            );
            assert!(run.finished.result.ran < secs(1));
            assert_eq!(run.signals(), [(core(3), Signal::ComputationError)]);
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn an_early_exit_is_an_unexpected_exit_of_that_core() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(|_, _| {
                Launch::Run(script(|ctx: ProcCtx| async move {
                    ctx.exit(ExitInfo::Code(0));
                }))
            });
            let handle = rig.start(spec(Regime::R2, &[1], secs(60))).await.unwrap();
            let run = finish(&handle).await;
            let failure = run.finished.result.failure.unwrap();
            assert_eq!(
                (failure.signal, failure.core),
                (Signal::UnexpectedExit, core(1))
            );
            assert_eq!(run.signals(), [(core(1), Signal::UnexpectedExit)]);
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn a_watched_file_error_is_found_at_the_next_tick() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let mut backend = TestBackend::new("/bin/true");
            backend.watched = vec!["results.txt"];
            let rig = rig_with(backend, Timing::default());
            rig.fake.on_launch(|_, command| {
                let results = command.cwd.join("results.txt");
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(results, "fine\nCOMPUTE ERROR\n").unwrap();
                    ctx.until_killed().await;
                }))
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(60))).await.unwrap();
            let run = finish(&handle).await;
            assert_eq!(
                run.finished.result.failure.map(|f| f.signal),
                Some(Signal::ComputationError)
            );
            assert!(run.finished.result.ran <= secs(2));
        })
        .await;
    });
}

#[test]
fn a_worker_that_stops_advancing_stalls_after_grace_plus_window() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[0], secs(120))).await.unwrap();
            let run = finish(&handle).await;
            let failure = run.finished.result.failure.unwrap();
            assert_eq!((failure.signal, failure.core), (Signal::Stall, core(0)));
            let ran = run.finished.result.ran;
            assert!(ran >= secs(40) && ran <= secs(41), "{ran:?}");
            assert!(run.warnings().iter().any(|w| w.starts_with("stalled: ")));
            assert_eq!(run.signals(), [(core(0), Signal::Stall)]);
        })
        .await;
    });
}

#[test]
fn a_worker_advancing_at_half_a_cpu_per_cpu_is_healthy() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R5, &[0], secs(60))).await.unwrap();
            burn(&rig.fake.process(0), 1);
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.failure, None);
            assert_eq!(run.finished.result.ran, secs(60));
        })
        .await;
    });
}

#[test]
fn cancelling_ends_the_trial_with_a_cancelled_error_and_a_confirmed_teardown() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let cancel = CancellationToken::new();
            let handle = rig
                .start_with(&cancel, spec(Regime::R1, &[0], secs(600)))
                .await
                .unwrap();
            tokio::time::sleep(secs(3)).await;
            cancel.cancel();
            let run = finish(&handle).await;
            assert!(matches!(run.finished.errors[..], [Error::Cancelled]));
            assert!(run.finished.teardown.is_ok());
            assert!(run.finished.result.ran >= secs(3) && run.finished.result.ran < secs(4));
            assert_eq!(run.finished.result.failure, None);
        })
        .await;
    });
}

#[test]
fn r4_load_steps_signal_the_group_and_count_stops_and_continues() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let mut trial = spec(Regime::R4, &[0], Duration::from_secs(1));
            trial.workload = togi_machine::pick_workload(Regime::R4, 0).id();
            let schedule = LoadSchedule::for_spec(&trial).unwrap();
            let handle = rig.start(trial).await.unwrap();
            let run = finish(&handle).await;
            let expected = schedule.toggles(secs(1));
            assert_eq!(
                (run.finished.result.stops, run.finished.result.conts),
                (expected.stops, expected.conts)
            );
            assert!(expected.stops > 0 && expected.conts > 0);
            let signals = rig.fake.process(0).signals.borrow().clone();
            assert_eq!(signals[0], GroupSignal::Stop);
            assert_eq!(signals[1], GroupSignal::Cont);
            assert_eq!(handle.started.schedule, Some(schedule));
        })
        .await;
    });
}

#[test]
fn r7_runs_one_instance_per_core_and_progress_names_the_core() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(printing("ok working\n"));
            let handle = rig.start(spec(Regime::R7, &[1, 5], secs(3))).await.unwrap();
            assert_eq!(handle.started.instances.len(), 2);
            assert_eq!(handle.started.instances[0].scope, "togi-trial-t1-c01");
            assert_eq!(handle.started.instances[1].scope, "togi-trial-t1-c05");
            assert_eq!(handle.started.instances[1].cpus, [CpuId(5)]);
            let run = finish(&handle).await;
            let mut progress = run.progress();
            progress.sort_unstable();
            assert_eq!(progress, ["core 01 ok working", "core 05 ok working"]);
            assert_eq!(run.finished.result.failure, None);
            assert_eq!(
                (run.finished.result.stops, run.finished.result.conts),
                (0, 0)
            );
        })
        .await;
    });
}

#[test]
fn a_thread_on_a_foreign_cpu_is_an_escape_and_ends_the_trial() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[2], secs(60))).await.unwrap();
            *rig.fake.process(0).threads.borrow_mut() = Ok(Threads {
                threads: vec![Thread { tid: 7, cpu: 3 }, Thread { tid: 8, cpu: 2 }],
                errors: Vec::new(),
            });
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.escaped, [CpuId(3)]);
            assert_eq!(run.finished.result.failure, None);
            assert!(run.warnings().contains(&"outside allowed cpus"));
            assert!(run.finished.result.ran <= secs(2));
        })
        .await;
    });
}

#[test]
fn a_scope_that_is_never_entered_makes_the_trial_inconclusive_not_failed() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[0], secs(5))).await.unwrap();
            rig.fake.process(0).ready.set(false);
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.failure, None);
            assert_eq!(
                run.finished.result.inconclusive.as_deref(),
                Some("core 00 setup error: never entered scope togi-trial-t1")
            );
            assert_eq!(run.finished.result.ran, secs(5));
        })
        .await;
    });
}

#[test]
fn a_lost_sample_sticks_through_the_whole_trial() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[0], secs(5))).await.unwrap();
            let process = rig.fake.process(0);
            *process.usage.borrow_mut() = Err(lost_read());
            tokio::time::sleep(Duration::from_millis(1500)).await;
            *process.usage.borrow_mut() = Ok(crate::procfs::Usage {
                cpu_time: secs(1),
                cpu: 0,
            });
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.ran, secs(5));
            assert_eq!(
                run.finished.result.inconclusive.as_deref(),
                Some("core 00 usage sampling lost: read /proc/fake/stat: Permission denied (os error 13)")
            );
        })
        .await;
    });
}

#[test]
fn a_disappearing_process_is_not_a_sampling_loss() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[0], secs(5))).await.unwrap();
            let process = rig.fake.process(0);
            *process.usage.borrow_mut() = Err(ProcError::Gone);
            *process.threads.borrow_mut() = Err(ProcError::Gone);
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.inconclusive, None);
            assert_eq!(run.finished.result.ran, secs(5));
        })
        .await;
    });
}

#[test]
fn a_partial_thread_listing_still_proves_an_escape_despite_other_failures() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[0], secs(30))).await.unwrap();
            *rig.fake.process(0).threads.borrow_mut() = Ok(Threads {
                threads: vec![Thread { tid: 9, cpu: 6 }],
                errors: vec![lost_read()],
            });
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.escaped, [CpuId(6)]);
            assert_eq!(run.finished.result.inconclusive, None);
        })
        .await;
    });
}

#[derive(Clone, Copy, Debug)]
enum Read {
    Threads,
    Usage,
}

fn lose(read: Read, process: &ProcShared) {
    match read {
        Read::Threads => *process.threads.borrow_mut() = Err(lost_read()),
        Read::Usage => *process.usage.borrow_mut() = Err(lost_read()),
    }
}

fn recover(read: Read, process: &ProcShared) {
    match read {
        Read::Threads => {
            *process.threads.borrow_mut() = Ok(Threads {
                threads: Vec::new(),
                errors: Vec::new(),
            });
        }
        Read::Usage => {
            *process.usage.borrow_mut() = Ok(Usage {
                cpu_time: secs(1),
                cpu: 0,
            });
        }
    }
}

#[test_case(Read::Threads, false, "core 00 thread sampling lost: read /proc/fake/stat: Permission denied (os error 13)"; "threads persistent")]
#[test_case(Read::Threads, true, "core 00 thread sampling lost: read /proc/fake/stat: Permission denied (os error 13)"; "threads transient")]
#[test_case(Read::Usage, false, "core 00 usage sampling lost: read /proc/fake/stat: Permission denied (os error 13)"; "usage persistent")]
#[test_case(Read::Usage, true, "core 00 usage sampling lost: read /proc/fake/stat: Permission denied (os error 13)"; "usage transient")]
fn a_lost_sampling_read_makes_the_whole_trial_inconclusive_and_teardown_still_confirms(
    read: Read,
    transient: bool,
    reason: &str,
) {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[0], secs(5))).await.unwrap();
            let process = rig.fake.process(0);
            lose(read, &process);
            if transient {
                // One tick (at 1 s) meets the loss; the reads are healthy again afterwards.
                tokio::time::sleep(Duration::from_millis(1500)).await;
                recover(read, &process);
            }
            let run = finish(&handle).await;
            let result = &run.finished.result;
            assert_eq!(result.ran, secs(5));
            assert_eq!(result.failure, None);
            assert_eq!(result.inconclusive.as_deref(), Some(reason));
            assert!(run.finished.errors.is_empty());
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

/// The fake OS, counting the thread reads that failed, so a test can prove the supervisor met a
/// loss before the evidence that must outlive it.
struct Probe {
    inner: Fake,
    lost_threads: Rc<Cell<u32>>,
}

impl Os for Probe {
    type Child = <Fake as Os>::Child;

    fn launch(
        &self,
        command: &crate::launch::LaunchCommand,
    ) -> Result<Self::Child, LaunchFailure<Self::Child>> {
        self.inner.launch(command)
    }

    fn in_scope(&self, pid: u32, scope: &ScopeName) -> Result<bool, ProcError> {
        self.inner.in_scope(pid, scope)
    }

    fn usage(&self, pid: u32) -> Result<Usage, ProcError> {
        self.inner.usage(pid)
    }

    fn threads(&self, pid: u32) -> Result<Threads, ProcError> {
        let read = self.inner.threads(pid);
        if matches!(&read, Err(error) if !matches!(error, ProcError::Gone)) {
            self.lost_threads.set(self.lost_threads.get() + 1);
        }
        read
    }

    async fn systemd(
        &self,
        operation: &Systemd,
        deadline: Instant,
    ) -> Result<togi_command::Output, togi_command::Error> {
        self.inner.systemd(operation, deadline).await
    }

    async fn scope_processes(&self, deadline: Instant) -> Result<Vec<ScopeProcess>, ProcError> {
        self.inner.scope_processes(deadline).await
    }

    async fn survivors(
        &self,
        captured: &[ScopeProcess],
        deadline: Instant,
    ) -> Result<usize, ProcError> {
        self.inner.survivors(captured, deadline).await
    }
}

/// A trial environment over `Probe`, sharing the fake and the fixture directory of `rig`.
fn probe_env(rig: &Rig) -> (Rc<Env<Probe>>, Rc<Cell<u32>>) {
    let lost_threads = Rc::new(Cell::new(0));
    let env = Env {
        os: Rc::new(Probe {
            inner: rig.fake.clone(),
            lost_threads: Rc::clone(&lost_threads),
        }),
        timing: rig.env.timing,
        roots: rig.env.roots.clone(),
        inputs: rig.env.inputs.clone(),
        durability: rig.env.durability,
        memory: rig.env.memory.clone(),
        dir: rig.env.dir.clone(),
        backends: rig.env.backends.clone(),
        topology: rig.env.topology.clone(),
        identity: rig.env.identity,
        unscoped: rig.env.unscoped,
        user_manager: rig.env.user_manager,
        pm_table: rig.env.pm_table.clone(),
        storage: rig.env.storage.clone(),
        log_sink: rig.env.log_sink.clone(),
        supervisors: std::cell::RefCell::default(),
        preparer: rig.env.preparer.clone(),
        controller: Arc::clone(&rig.env.controller),
    };
    (Rc::new(env), lost_threads)
}

#[test]
fn a_computation_error_that_follows_a_thread_read_loss_is_not_replaced_by_the_loss() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let (env, lost_threads) = probe_env(&rig);
            rig.fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    tokio::time::sleep(Duration::from_millis(2500)).await;
                    ctx.print("COMPUTE ERROR\n").await;
                    ctx.until_killed().await;
                }))
            });
            let handle = start(
                env,
                CancellationToken::new(),
                spec(Regime::R1, &[0], secs(30)),
                nothing_runs(),
            )
            .await
            .unwrap();
            *rig.fake.process(0).threads.borrow_mut() = Err(lost_read());
            let run = finish(&handle).await;
            assert!(
                lost_threads.get() >= 2,
                "the loss was met at the ticks before the error: {}",
                lost_threads.get()
            );
            assert_eq!(
                run.finished.result.failure.map(|f| f.signal),
                Some(Signal::ComputationError)
            );
            assert_eq!(run.finished.result.inconclusive, None);
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn a_stall_that_follows_a_thread_read_loss_is_not_replaced_by_the_loss() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let (env, lost_threads) = probe_env(&rig);
            let handle = start(
                env,
                CancellationToken::new(),
                spec(Regime::R1, &[0], secs(120)),
                nothing_runs(),
            )
            .await
            .unwrap();
            *rig.fake.process(0).threads.borrow_mut() = Err(lost_read());
            let run = finish(&handle).await;
            assert!(
                lost_threads.get() >= 30,
                "the loss was met at every tick up to the stall: {}",
                lost_threads.get()
            );
            assert_eq!(
                run.finished.result.failure.map(|f| f.signal),
                Some(Signal::Stall)
            );
            // The stall stays the trial's signal; the loss that came before it stays recorded
            // beside it rather than replacing it.
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn an_exit_queued_before_a_cancellation_is_still_an_unexpected_exit() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(|_, _| {
                Launch::Run(script(|ctx: ProcCtx| async move {
                    ctx.exit(ExitInfo::Code(3));
                }))
            });
            let cancel = CancellationToken::new();
            let handle = rig
                .start_with(&cancel, spec(Regime::R1, &[2], secs(60)))
                .await
                .unwrap();
            cancel.cancel();
            let run = finish(&handle).await;
            assert!(matches!(run.finished.errors[..], [Error::Cancelled]));
            let failure = run.finished.result.failure.unwrap();
            assert_eq!(
                (failure.signal, failure.core),
                (Signal::UnexpectedExit, core(2))
            );
        })
        .await;
    });
}

/// A worker that floods stdout with short lines until it is terminated.
fn flooding(_: usize, _: &crate::launch::LaunchCommand) -> Launch {
    Launch::Run(script(|mut ctx: ProcCtx| async move {
        loop {
            while let Ok(delivery) = ctx.signals.try_recv() {
                if matches!(
                    delivery,
                    crate::os::fake::Delivery::Group(GroupSignal::Term | GroupSignal::Kill)
                        | crate::os::fake::Delivery::Scope(_)
                ) {
                    ctx.exit(ExitInfo::Signal(15));
                    return;
                }
            }
            ctx.print(&"ok\n".repeat(4096)).await;
            tokio::task::yield_now().await;
        }
    }))
}

#[test]
fn an_output_flood_neither_skips_load_steps_nor_delays_the_deadline() {
    crate::testing::runtime(false).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(flooding);
            let trial = spec(Regime::R4, &[0], secs(1));
            let schedule = LoadSchedule::for_spec(&trial).unwrap();
            let handle = rig.start(trial).await.unwrap();
            let started = tokio::time::Instant::now();
            let run = finish(&handle).await;
            let ended = tokio::time::Instant::now();
            let term = rig
                .fake
                .process(0)
                .signal_times
                .borrow()
                .iter()
                .find(|(_, signal)| matches!(signal, GroupSignal::Term))
                .map(|(at, _)| at.duration_since(started))
                .expect("the teardown terminated the flooding worker");
            let expected = schedule.toggles(secs(1));
            assert_eq!(
                (run.finished.result.stops, run.finished.result.conts),
                (expected.stops, expected.conts)
            );
            let timing = rig.env.timing;
            assert!(
                term < secs(1) + Duration::from_millis(500),
                "the flood delayed the deadline: {term:?}"
            );
            assert!(
                ended.duration_since(started) < timing.wait_bound(secs(1)),
                "the end outlived the spec's bound: {:?}",
                ended.duration_since(started)
            );
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The test backend with a classifier, a check or a read of its own that misbehaves on demand.
#[derive(Debug)]
struct Odd {
    inner: TestBackend,
    /// Blocks the thread that classifies a line containing `SLOW` for this long.
    nap: Duration,
    /// Panics on a line containing this text.
    panic_on: Option<&'static str>,
    fail_check: bool,
}

impl Odd {
    fn new(inner: TestBackend) -> Self {
        Self {
            inner,
            nap: Duration::ZERO,
            panic_on: None,
            fail_check: false,
        }
    }

    fn rig(self) -> Rig {
        rig_edit(self.inner.clone(), Timing::default(), |env| {
            env.backends = Backends::new().with(Arc::new(self));
        })
    }
}

impl Backend for Odd {
    fn kind(&self) -> togi_machine::Backend {
        self.inner.kind()
    }

    fn check(&self) -> Result<Ready, togi_backend::Error> {
        if self.fail_check {
            return Err(togi_backend::Error::NotExecutable {
                name: "test",
                path: "/test/backend".into(),
            });
        }
        self.inner.check()
    }

    fn plan(
        &self,
        workload: &Workload,
        dir: &Path,
        cpus: &[CpuId],
    ) -> Result<Plan, togi_backend::Error> {
        self.inner.plan(workload, dir, cpus)
    }

    fn classify(&self, line: &str) -> Line {
        if line.contains("SLOW") {
            std::thread::sleep(self.nap);
        }
        if let Some(text) = self.panic_on {
            assert!(!line.contains(text), "the classifier met {text}");
        }
        self.inner.classify(line)
    }
}

#[test]
fn a_watched_poll_that_uses_the_rest_of_the_duration_leaves_no_required_sample_after_it() {
    crate::testing::runtime(false).block_on(async move {
        local(async {
            let mut inner = TestBackend::new("/bin/true");
            inner.watched = vec!["results.txt"];
            let mut odd = Odd::new(inner.clone());
            odd.nap = ms(150);
            let timing = Timing {
                sample: ms(100),
                ..Timing::default()
            };
            let rig = rig_edit(inner, timing, |env| {
                env.backends = Backends::new().with(Arc::new(odd));
            });
            rig.fake.on_launch(|_, command| {
                let results = command.cwd.join("results.txt");
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(results, "SLOW one\nSLOW two\n").unwrap();
                    ctx.until_killed().await;
                }))
            });
            let handle = rig.start(spec(Regime::R1, &[0], ms(200))).await.unwrap();
            // A thread on a CPU the instance was not given: read after the deadline it would be an
            // escape that ends the trial and overrides what the teardown recovers.
            *rig.fake.process(0).threads.borrow_mut() = Ok(Threads {
                threads: vec![Thread { tid: 9, cpu: 6 }],
                errors: Vec::new(),
            });
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.escaped, []);
            assert_eq!(run.finished.result.ran, ms(200));
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn a_watched_poll_that_uses_the_rest_of_the_duration_still_drains_a_quiet_instances_final_error() {
    crate::testing::runtime(false).block_on(async move {
        local(async {
            let mut inner = TestBackend::new("/bin/true");
            inner.watched = vec!["results.txt"];
            let mut odd = Odd::new(inner.clone());
            odd.nap = ms(150);
            let timing = Timing {
                sample: ms(100),
                ..Timing::default()
            };
            let rig = rig_edit(inner, timing, |env| {
                env.backends = Backends::new().with(Arc::new(odd));
            });
            // The first instance keeps the poll busy past the deadline; the second is quiet and
            // leaves a computation error without its line terminator.
            rig.fake.on_launch(|index, command| {
                let results = command.cwd.join("results.txt");
                let text = if index == 0 {
                    "SLOW one\nSLOW two\n"
                } else {
                    "COMPUTE ERROR"
                };
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(results, text).unwrap();
                    ctx.until_killed().await;
                }))
            });
            let handle = rig.start(spec(Regime::R7, &[1, 5], ms(200))).await.unwrap();
            // Threads on a CPU neither instance was given: sampled after the deadline they would be an
            // escape that ends the trial and overrides the evidence the teardown recovers.
            for index in 0..2 {
                *rig.fake.process(index).threads.borrow_mut() = Ok(Threads {
                    threads: vec![Thread { tid: 9, cpu: 6 }],
                    errors: Vec::new(),
                });
            }
            let run = finish(&handle).await;
            let result = &run.finished.result;
            let failure = result.failure.unwrap();
            assert_eq!(
                (failure.signal, failure.core),
                (Signal::ComputationError, core(5))
            );
            assert_eq!(result.escaped, []);
            assert_eq!(result.inconclusive, None);
            assert_eq!(result.ran, ms(200));
            assert_eq!(run.signals(), [(core(5), Signal::ComputationError)]);
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

/// Every error of a joined start failure, in order.
fn joined(error: &togi_machine::Error) -> Vec<&Error> {
    if let Some(Error::Joined(joined)) = crate_error(error) {
        joined.errors().iter().collect()
    } else {
        Vec::new()
    }
}

/// A log sink whose every write fails, as a full disk does.
struct Refusing;

impl io::Write for Refusing {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("disk full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn refusing_sink() -> Rc<dyn Fn() -> Wrap> {
    Rc::new(|| Box::new(|_: Sink| Box::new(Refusing) as Sink))
}

/// A log sink whose writes block until the sender is dropped.
struct Stuck(Arc<Mutex<Receiver<()>>>);

impl io::Write for Stuck {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let _ = self.0.lock().unwrap().recv();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn stuck_sink() -> (Sender<()>, Rc<dyn Fn() -> Wrap>) {
    let (release, gate) = channel();
    let gate = Arc::new(Mutex::new(gate));
    let sink: Rc<dyn Fn() -> Wrap> = Rc::new(move || {
        let gate = Arc::clone(&gate);
        Box::new(move |_: Sink| Box::new(Stuck(Arc::clone(&gate))) as Sink)
    });
    (release, sink)
}

/// The first instance prints and runs; the second cannot be launched.
fn second_launch_fails() -> impl FnMut(usize, &crate::launch::LaunchCommand) -> Launch {
    let mut first = printing("hello\n");
    move |index, command| {
        if index == 0 {
            first(index, command)
        } else {
            Launch::Fail(io::Error::other("no process left"))
        }
    }
}

#[test]
fn a_partial_start_joins_the_output_log_failure_with_the_start_error() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
                env.log_sink = Some(refusing_sink());
            });
            rig.fake.on_launch(second_launch_fails());
            let partial = rig
                .start(spec(Regime::R7, &[1, 5], secs(30)))
                .await
                .err()
                .unwrap();
            assert!(partial.value.is_some(), "the rollback confirmed the stop");
            assert!(rig.fake.process(0).reaped.get());
            let errors = joined(&partial.error);
            assert!(
                matches!(errors.first(), Some(Error::Launch { .. })),
                "{errors:?}"
            );
            assert!(
                matches!(errors.get(1), Some(Error::Output(failure))
            if failure.diagnostics.iter().any(|d| d.contains("write output log: disk full"))),
                "{errors:?}"
            );
            let Some(Error::Joined(joined)) = crate_error(&partial.error) else {
                panic!("{:?}", partial.error);
            };
            let expected = "launch instance c05 in scope togi-trial-t1-c05: no process left; backend output could not be read cleanly: core 01 stdout.log: write output log: disk full";
            assert_eq!(joined.to_string(), expected);
            assert_eq!(partial.error.to_string(), expected);
        })
        .await;
    });
}

#[test]
fn a_partial_start_reports_a_log_writer_that_will_not_finish() {
    crate::testing::runtime(false).block_on(async move {
        local(async {
            let (release, sink) = stuck_sink();
            let timing = Timing {
                writer_join: ms(100),
                ..Timing::default()
            };
            let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
                env.log_sink = Some(sink);
            });
            rig.fake.on_launch(second_launch_fails());
            let partial = rig
                .start(spec(Regime::R7, &[1, 5], secs(30)))
                .await
                .err()
                .unwrap();
            assert!(partial.value.is_some());
            let errors = joined(&partial.error);
            assert!(
                matches!(errors.first(), Some(Error::Launch { .. })),
                "{errors:?}"
            );
            assert!(
                matches!(errors.get(1), Some(Error::Output(failure))
            if failure.diagnostics.iter().any(|d| d.contains("did not finish within"))),
                "{errors:?}"
            );
            drop(release);
        })
        .await;
    });
}

#[test]
fn a_failing_log_sink_reaches_wait_with_the_backend_evidence_and_a_confirmed_teardown() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
                env.log_sink = Some(refusing_sink());
            });
            rig.fake.on_launch(printing("COMPUTE ERROR\n"));
            let handle = rig.start(spec(Regime::R1, &[0], secs(30))).await.unwrap();
            let run = finish(&handle).await;
            assert!(run.finished.teardown.is_ok());
            assert_eq!(
                run.finished.result.failure.map(|f| f.signal),
                Some(Signal::ComputationError)
            );
            assert!(
                matches!(&run.finished.errors[..], [Error::Output(failure)]
            if failure.diagnostics.iter().any(|d| d.contains("write output log: disk full"))),
                "{:?}",
                run.finished.errors
            );
        })
        .await;
    });
}

#[test]
fn a_backend_that_fails_its_check_is_refused_with_its_error_and_nothing_launches() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let mut odd = Odd::new(TestBackend::new("/bin/true"));
            odd.fail_check = true;
            let rig = odd.rig();
            let partial = rig
                .start(spec(Regime::R1, &[0], secs(5)))
                .await
                .err()
                .unwrap();
            assert!(partial.value.is_some(), "the stop proof is returned");
            assert!(matches!(
                &partial.error,
                togi_machine::Error::Other(inner)
                    if matches!(
                        inner.downcast_ref::<togi_backend::Error>(),
                        Some(togi_backend::Error::NotExecutable { .. })
                    )
            ));
            assert_eq!(rig.fake.process_count(), 0);
            assert!(rig.fake.launches().is_empty());
        })
        .await;
    });
}

#[test]
fn a_start_that_is_already_cancelled_returns_the_cancellation_and_launches_nothing() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let cancel = CancellationToken::new();
            cancel.cancel();
            let partial = rig
                .start_with(&cancel, spec(Regime::R1, &[0], secs(5)))
                .await
                .err()
                .unwrap();
            assert!(partial.value.is_some(), "the stop proof is returned");
            assert!(matches!(partial.error, togi_machine::Error::Cancelled));
            assert_eq!(rig.fake.process_count(), 0);
            assert!(rig.fake.launches().is_empty());
        })
        .await;
    });
}

#[test]
fn a_duration_whose_deadlines_overflow_is_refused_before_anything_launches() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let partial = rig
                .start(spec(Regime::R1, &[0], Duration::MAX))
                .await
                .err()
                .unwrap();
            assert!(partial.value.is_some());
            assert!(matches!(
                crate_error(&partial.error),
                Some(Error::Invalid(InvalidTrial::UnrepresentableDuration))
            ));
            assert_eq!(rig.fake.process_count(), 0);
        })
        .await;
    });
}

fn with_inputs() -> TestBackend {
    let mut backend = TestBackend::new("/bin/true");
    backend.inputs = vec![("prime.txt", "a"), ("local.txt", "b")];
    backend
}

fn refusing_ownership_of(target: &'static str) -> Chown {
    Chown::new(move |name, _, _, _| {
        if name == target {
            Err(rustix::io::Errno::PERM)
        } else {
            Ok(())
        }
    })
}

/// The preparation error a failed start carries, first of the joined ones when there are several.
fn prepare_error(error: &togi_machine::Error) -> Option<&PrepareError> {
    let error = crate_error(error)?;
    if let Error::Prepare(error) = error {
        Some(error)
    } else if let Error::Joined(joined) = error
        && let Some(Error::Prepare(error)) = joined.errors().first()
    {
        Some(error)
    } else {
        None
    }
}

#[test]
fn an_ownership_failure_names_the_input_and_launches_nothing() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
    let rig = rig_edit(with_inputs(), Timing::default(), |env| {
        env.preparer.chown = refusing_ownership_of("prime.txt");
    });
    let partial = rig
        .start(spec(Regime::R1, &[0], secs(5)))
        .await
        .err()
        .unwrap();
    assert!(partial.value.is_some(), "the stop proof is returned");
    assert!(
        matches!(prepare_error(&partial.error), Some(PrepareError::Chown { instance, name, .. })
            if instance == "work" && name == "prime.txt"),
        "{:?}",
        partial.error
    );
    assert!(rig.fake.launches().is_empty());
    assert_eq!(rig.fake.process_count(), 0);
})
.await;
    });
}

#[test]
fn an_ownership_failure_of_a_later_instance_rolls_the_earlier_one_back() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_edit(with_inputs(), Timing::default(), |env| {
                env.preparer.chown = refusing_ownership_of("c01");
            });
            let partial = rig
                .start(spec(Regime::R7, &[0, 1], secs(30)))
                .await
                .err()
                .unwrap();
            assert!(partial.value.is_some(), "the rollback confirmed the stop");
            assert!(
                matches!(prepare_error(&partial.error), Some(PrepareError::Chown { instance, .. })
            if instance == "c01"),
                "{:?}",
                partial.error
            );
            assert_eq!(rig.fake.launches().len(), 1, "c01 never launched");
            assert!(rig.fake.process(0).reaped.get());
        })
        .await;
    });
}

/// A preparation that blocks on the preparation thread, as a stuck filesystem write does, the
/// first time it reaches `step`, until released.
struct BlockedPreparation {
    entered: Arc<tokio::sync::Notify>,
    release: Sender<()>,
}

impl BlockedPreparation {
    /// Whether the preparation thread entered the step within `bound` of real time. The held
    /// clock keeps paused time from jumping past a thread that runs in real time.
    async fn entered(&self, bound: Duration) -> bool {
        let _clock = crate::join::ClockHold::new(bound);
        tokio::time::timeout(bound, self.entered.notified())
            .await
            .is_ok()
    }

    /// Starts `spec`, cancelling the start once the preparation entered the step or `bound` of
    /// real time passed without it; `None` is the entry wait that the start outlasted.
    async fn start_cancelled_on_entry(
        &self,
        rig: &Rig,
        spec: TrialSpec,
        bound: Duration,
    ) -> (Result<Handle, Partial<Option<Stopped>>>, Option<bool>) {
        let cancel = CancellationToken::new();
        let start = rig.start_with(&cancel, spec);
        let entry = self.entered(bound);
        tokio::pin!(start, entry);
        let mut entered = None;
        loop {
            tokio::select! {
                result = &mut start => return (result, entered),
                seen = &mut entry, if entered.is_none() => {
                    entered = Some(seen);
                    cancel.cancel();
                }
            }
        }
    }
}

/// Releases the blocked preparation if the operation it guards outlives `bound` of real time, so
/// a regression ends in a failed assertion instead of a hang. Dropping it ends the watch.
struct WallBound {
    fired: Arc<AtomicBool>,
    _done: Sender<()>,
}

impl WallBound {
    fn new(release: Sender<()>, bound: Duration) -> Self {
        let fired = Arc::new(AtomicBool::new(false));
        let (done, finished) = channel::<()>();
        let flag = Arc::clone(&fired);
        std::thread::spawn(move || {
            if finished.recv_timeout(bound) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                flag.store(true, Ordering::Release);
                let _ = release.send(());
            }
        });
        Self { fired, _done: done }
    }

    fn fired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }
}

fn blocked_preparation(step: &'static str, prepare: Duration) -> (Rig, BlockedPreparation) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let (release, release_rx) = channel();
    let release_rx = Mutex::new(release_rx);
    let first = AtomicBool::new(true);
    let signal = Arc::clone(&entered);
    let hook = move |name: &str| {
        if name == step && first.swap(false, Ordering::AcqRel) {
            signal.notify_one();
            let _ = release_rx.lock().unwrap().recv();
        }
    };
    let timing = Timing {
        prepare,
        ..Timing::default()
    };
    let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
        env.preparer.entered = Some(Arc::new(hook));
    });
    (rig, BlockedPreparation { entered, release })
}

#[test]
fn a_preparation_stuck_past_its_budget_ends_the_start_with_a_typed_error() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (rig, blocked) = blocked_preparation("the trial directory", ms(300));
            let partial = rig
                .start(spec(Regime::R1, &[0], secs(5)))
                .await
                .err()
                .unwrap();
            assert!(partial.value.is_some(), "nothing ran, so the proof returns");
            assert!(
                matches!(prepare_error(&partial.error), Some(PrepareError::TimedOut { what, bound })
            if what == "the trial directory" && *bound == ms(300)),
                "{:?}",
                partial.error
            );
            assert!(
                blocked.entered(secs(5)).await,
                "the preparation never entered"
            );
            assert!(rig.fake.launches().is_empty());
            blocked.release.send(()).unwrap();
        })
        .await;
    });
}

#[test]
fn cancelling_while_a_preparation_is_stuck_ends_the_start_without_waiting_for_it() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (rig, blocked) = blocked_preparation("the trial directory", secs(30));
            let watch = WallBound::new(blocked.release.clone(), secs(30));
            let (result, entered) = blocked
                .start_cancelled_on_entry(&rig, spec(Regime::R1, &[0], secs(5)), secs(10))
                .await;
            assert!(
                !watch.fired(),
                "the cancelled start waited for the stuck thread"
            );
            assert_eq!(entered, Some(true), "the preparation never entered");
            let partial = result.err().unwrap();
            assert!(matches!(partial.error, togi_machine::Error::Cancelled));
            assert!(partial.value.is_some());
            assert!(rig.fake.launches().is_empty());
            blocked.release.send(()).unwrap();
        })
        .await;
    });
}

#[test]
fn an_entry_that_never_comes_is_a_finite_failure() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (rig, blocked) = blocked_preparation("a step nothing runs", secs(30));
            let watch = WallBound::new(blocked.release.clone(), secs(30));
            let (result, entered) = blocked
                .start_cancelled_on_entry(&rig, spec(Regime::R1, &[0], secs(1)), ms(50))
                .await;
            assert!(!watch.fired());
            assert_eq!(entered, None, "the start ended before the entry wait did");
            let handle = result.unwrap();
            assert!(
                !blocked.entered(ms(50)).await,
                "an entry was reported for a step that never ran"
            );
            let run = finish(&handle).await;
            assert!(run.finished.errors.is_empty(), "{:?}", run.finished.errors);
        })
        .await;
    });
}

#[test]
fn a_start_refused_before_preparing_ends_the_entry_wait_without_an_entry() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (rig, blocked) = blocked_preparation("the trial directory", secs(30));
            let watch = WallBound::new(blocked.release.clone(), secs(30));
            let (result, entered) = blocked
                .start_cancelled_on_entry(&rig, spec(Regime::R1, &[0], Duration::MAX), secs(10))
                .await;
            assert!(!watch.fired(), "the refused start did not end");
            assert_eq!(entered, None);
            let partial = result.err().unwrap();
            assert!(matches!(
                crate_error(&partial.error),
                Some(Error::Invalid(InvalidTrial::UnrepresentableDuration))
            ));
            assert_eq!(rig.fake.process_count(), 0);
        })
        .await;
    });
}

#[test]
fn a_preparation_stuck_after_an_earlier_instance_launched_rolls_that_instance_back() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (rig, blocked) = blocked_preparation("c01", ms(300));
            let partial = rig
                .start(spec(Regime::R7, &[0, 1], secs(30)))
                .await
                .err()
                .unwrap();
            assert!(partial.value.is_some(), "the rollback confirmed the stop");
            assert!(
                matches!(prepare_error(&partial.error), Some(PrepareError::TimedOut { what, .. })
            if what == "c01"),
                "{:?}",
                partial.error
            );
            assert_eq!(rig.fake.launches().len(), 1);
            assert!(rig.fake.process(0).reaped.get());
            blocked.release.send(()).unwrap();
        })
        .await;
    });
}

#[test]
fn an_abandoned_preparation_blocks_the_next_one_until_it_returns() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (rig, blocked) = blocked_preparation("the trial directory", ms(300));
            let first = rig
                .start(spec(Regime::R1, &[0], secs(5)))
                .await
                .err()
                .unwrap();
            assert!(matches!(
                prepare_error(&first.error),
                Some(PrepareError::TimedOut { .. })
            ));
            let second = rig
                .start(spec(Regime::R1, &[0], secs(5)))
                .await
                .err()
                .unwrap();
            assert!(second.value.is_some());
            assert!(matches!(
                prepare_error(&second.error),
                Some(PrepareError::Busy)
            ));
            blocked.release.send(()).unwrap();
            let began = std::time::Instant::now();
            let handle = loop {
                match rig.start(spec(Regime::R1, &[0], secs(5))).await {
                    Ok(handle) => break handle,
                    Err(partial)
                        if matches!(prepare_error(&partial.error), Some(PrepareError::Busy))
                            && began.elapsed() < secs(10) =>
                    {
                        tokio::task::yield_now().await;
                    }
                    Err(partial) => panic!(
                        "the released preparation never freed the preparer: {}",
                        partial.error
                    ),
                }
            };
            handle.commands.send(TrialCommand::Stop).unwrap();
            assert!(finish(&handle).await.finished.teardown.is_ok());
        })
        .await;
    });
}

/// A reader as the real one behaves while its transfer is stuck: it returns at once with nothing
/// and starts one transfer, which never finishes.
#[derive(Default)]
struct BlockedTransfer {
    reads: AtomicU32,
    transfers: AtomicU32,
    stuck: Mutex<Option<Sender<()>>>,
}

impl PmTableReader for BlockedTransfer {
    fn pm_table(&self) -> Option<PmTable> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let mut stuck = self.stuck.lock().unwrap();
        if stuck.is_none() {
            let (release, blocked) = channel::<()>();
            self.transfers.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                let _ = blocked.recv();
            });
            *stuck = Some(release);
        }
        None
    }
}

/// Samples kept in memory.
#[derive(Clone, Default)]
pub(super) struct Recorder {
    pub(super) lines: Arc<Mutex<Vec<Vec<u8>>>>,
    pub(super) closed: Arc<AtomicBool>,
}

impl Storage for Recorder {
    fn append(&mut self, line: &[u8]) -> io::Result<()> {
        self.lines.lock().unwrap().push(line.to_vec());
        Ok(())
    }

    fn close(self: Box<Self>) -> io::Result<()> {
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}

impl Recorder {
    pub(super) fn storage(&self) -> Rc<dyn Fn() -> Result<Open, crate::error::StorageError>> {
        let recorder = self.clone();
        Rc::new(move || {
            let recorder = recorder.clone();
            Ok(Box::new(move || Ok(Box::new(recorder) as Box<dyn Storage>)) as Open)
        })
    }

    pub(super) fn samples(&self) -> Vec<serde_json::Value> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect()
    }
}

struct Blocked {
    rig: Rig,
    reader: Arc<BlockedTransfer>,
    recorder: Recorder,
}

fn blocked_pm() -> Blocked {
    let reader = Arc::new(BlockedTransfer::default());
    let recorder = Recorder::default();
    let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
        env.pm_table = Some(Arc::clone(&reader) as Arc<dyn PmTableReader + Send + Sync>);
        env.storage = Some(recorder.storage());
    });
    Blocked {
        rig,
        reader,
        recorder,
    }
}

impl Blocked {
    /// A transfer that stays blocked is started once and its lanes never reach a sample.
    fn assert_single_flight_and_no_lanes(&self) {
        assert_eq!(self.reader.transfers.load(Ordering::Relaxed), 1);
        assert!(self.reader.reads.load(Ordering::Relaxed) >= 1);
        let samples = self.recorder.samples();
        assert!(!samples.is_empty());
        assert!(
            samples
                .iter()
                .all(|sample| sample.get("pm_table").is_none())
        );
    }
}

#[test]
fn a_blocked_pm_transfer_neither_delays_the_deadline_nor_puts_lanes_in_samples() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let blocked = blocked_pm();
            let handle = blocked
                .rig
                .start(spec(Regime::R1, &[0], secs(10)))
                .await
                .unwrap();
            let begun = Instant::now();
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.ran, secs(10));
            assert!(begun.elapsed() < secs(11));
            assert!(run.finished.errors.is_empty());
            assert!(run.finished.teardown.is_ok());
            blocked.assert_single_flight_and_no_lanes();
            assert!(blocked.reader.reads.load(Ordering::Relaxed) >= 8);
        })
        .await;
    });
}

#[test]
fn a_blocked_pm_transfer_does_not_delay_cancellation() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let blocked = blocked_pm();
            let cancel = CancellationToken::new();
            let handle = blocked
                .rig
                .start_with(&cancel, spec(Regime::R1, &[0], secs(600)))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(3500)).await;
            cancel.cancel();
            let run = finish(&handle).await;
            assert!(matches!(run.finished.errors[..], [Error::Cancelled]));
            assert_eq!(run.finished.result.ran, Duration::from_millis(3500));
            assert!(run.finished.teardown.is_ok());
            blocked.assert_single_flight_and_no_lanes();
        })
        .await;
    });
}

#[test]
fn a_blocked_pm_transfer_does_not_delay_a_computation_error() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let blocked = blocked_pm();
            blocked.rig.fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    tokio::time::sleep(Duration::from_millis(2500)).await;
                    ctx.print("COMPUTE ERROR\n").await;
                    ctx.until_killed().await;
                }))
            });
            let handle = blocked
                .rig
                .start(spec(Regime::R1, &[0], secs(600)))
                .await
                .unwrap();
            let run = finish(&handle).await;
            assert_eq!(
                run.finished.result.failure.map(|f| f.signal),
                Some(Signal::ComputationError)
            );
            assert_eq!(run.finished.result.ran, Duration::from_millis(2500));
            assert!(run.finished.teardown.is_ok());
            blocked.assert_single_flight_and_no_lanes();
        })
        .await;
    });
}

/// When each stop and continue of the first process was delivered, from the moment supervision
/// began.
fn toggles_after(rig: &Rig, begun: Instant) -> Vec<(u128, GroupSignal)> {
    rig.fake
        .process(0)
        .signal_times
        .borrow()
        .iter()
        .filter(|(at, signal)| {
            *at >= begun && matches!(signal, GroupSignal::Stop | GroupSignal::Cont)
        })
        .map(|(at, signal)| (at.duration_since(begun).as_millis(), *signal))
        .collect()
}

#[test]
fn a_blocked_pm_transfer_does_not_move_the_r6_burst_steps() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let duration = secs(10);
            let control = rig();
            let handle = control
                .start(spec(Regime::R6, &[0], duration))
                .await
                .unwrap();
            let begun = Instant::now();
            let expected_run = finish(&handle).await;
            let expected = toggles_after(&control, begun);

            let blocked = blocked_pm();
            let handle = blocked
                .rig
                .start(spec(Regime::R6, &[0], duration))
                .await
                .unwrap();
            let begun = Instant::now();
            let run = finish(&handle).await;
            assert!(!expected.is_empty(), "the bursts toggled the worker");
            assert_eq!(toggles_after(&blocked.rig, begun), expected);
            assert_eq!(
                (run.finished.result.stops, run.finished.result.conts),
                (
                    expected_run.finished.result.stops,
                    expected_run.finished.result.conts
                )
            );
            assert_eq!(run.finished.result.ran, duration);
            blocked.assert_single_flight_and_no_lanes();
        })
        .await;
    });
}

/// Asserts the supervisor published an end after a panic inside supervision, with the stop
/// confirmed and the panic reported rather than lost or turned into a clean result.
fn assert_panic_contained(rig: &Rig, run: &super::harness::Run, what: &str) {
    assert!(
        run.finished.teardown.is_ok(),
        "the teardown still proved the stop"
    );
    assert!(rig.fake.process(0).reaped.get(), "the worker was reaped");
    assert!(
        matches!(&run.finished.errors[..], [Error::Panicked { what: text }] if text.contains(what)),
        "{:?}",
        run.finished.errors
    );
    // Whatever else made the trial inconclusive first, a panicked supervision cannot pass.
    assert!(
        run.finished.result.inconclusive.is_some(),
        "{:?}",
        run.finished.result
    );
}

#[test]
fn a_classifier_panic_still_tears_the_trial_down_and_is_reported() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let mut odd = Odd::new(TestBackend::new("/bin/true"));
            odd.panic_on = Some("BOOM");
            let rig = odd.rig();
            rig.fake.on_launch(printing("BOOM\n"));
            let handle = rig.start(spec(Regime::R1, &[0], secs(600))).await.unwrap();
            let run = tokio::time::timeout(secs(300), finish(&handle))
                .await
                .expect("the supervisor published an end after the panic");
            assert_panic_contained(&rig, &run, "the classifier met BOOM");
        })
        .await;
    });
}

struct PanickingReader;

impl PmTableReader for PanickingReader {
    fn pm_table(&self) -> Option<PmTable> {
        panic!("the PM table reader failed");
    }
}

#[test]
fn a_pm_reader_panic_still_tears_the_trial_down_and_is_reported() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
                env.pm_table = Some(Arc::new(PanickingReader));
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(600))).await.unwrap();
            let run = tokio::time::timeout(secs(300), finish(&handle))
                .await
                .expect("the supervisor published an end after the panic");
            assert_panic_contained(&rig, &run, "the PM table reader failed");
        })
        .await;
    });
}

#[test]
fn a_handle_dropped_while_the_teardown_is_in_progress_is_still_reported_abandoned() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.until_killed_ignoring_term().await;
                }))
            });
            let cancel = CancellationToken::new();
            let handle = rig
                .start_with(&cancel, spec(Regime::R1, &[0], secs(600)))
                .await
                .unwrap();
            tokio::time::sleep(secs(3)).await;
            cancel.cancel();
            // The worker ignores SIGTERM, so the teardown is waiting out its grace.
            tokio::time::sleep(ms(500)).await;
            assert!(
                !rig.fake.process(0).reaped.get(),
                "the teardown is not over"
            );
            handle.commands.send(TrialCommand::Abandon).unwrap();
            let run = finish(&handle).await;
            assert!(run.finished.abandoned);
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn a_handle_that_stays_is_not_reported_abandoned() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[0], secs(2))).await.unwrap();
            let run = finish(&handle).await;
            assert!(!run.finished.abandoned);
        })
        .await;
    });
}

#[test]
fn a_usage_read_that_finds_the_exiting_process_gone_leaves_an_unexpected_exit_not_a_loss() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(|_, _| {
                Launch::Run(script(|ctx: ProcCtx| async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    ctx.exit(ExitInfo::Code(0));
                }))
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(5))).await.unwrap();
            *rig.fake.process(0).usage.borrow_mut() = Err(ProcError::Gone);
            let run = finish(&handle).await;
            assert_eq!(
                run.finished.result.failure.map(|f| f.signal),
                Some(Signal::UnexpectedExit)
            );
            assert_eq!(run.finished.result.inconclusive, None);
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}
