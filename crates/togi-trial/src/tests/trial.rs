use std::time::Duration;

use togi_backend::Backend as _;
use togi_machine::{CancellationToken, CpuId, LoadSchedule, Regime, Signal};

use super::harness::{burn, core, finish, local, printing, rig, rig_with, spec};
use crate::error::Error;
use crate::os::fake::{Launch, ProcCtx, script};
use crate::os::{ExitInfo, GroupSignal};
use crate::procfs::{ProcError, Thread, Threads};
use crate::testing::TestBackend;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[tokio::test(start_paused = true)]
async fn a_quiet_trial_runs_its_whole_duration_and_teardown_stops_it() {
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
}

#[tokio::test(start_paused = true)]
async fn progress_comes_from_stdout_and_stderr_progress_is_ignored() {
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
}

#[tokio::test(start_paused = true)]
async fn a_computation_error_ends_the_trial_and_names_the_core() {
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
}

#[tokio::test(start_paused = true)]
async fn an_early_exit_is_an_unexpected_exit_of_that_core() {
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
}

#[tokio::test(start_paused = true)]
async fn a_watched_file_error_is_found_at_the_next_tick() {
    local(async {
        let mut backend = TestBackend::new("/bin/true");
        backend.watched = vec!["results.txt"];
        let rig = rig_with(backend, Timing::default());
        rig.fake.on_launch(|_, command| {
            let results = command.cwd.join("results.txt");
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                std::fs::write(results, "fine\nCOMPUTE ERROR\n").unwrap();
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
}

#[tokio::test(start_paused = true)]
async fn a_worker_that_stops_advancing_stalls_after_grace_plus_window() {
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
}

#[tokio::test(start_paused = true)]
async fn a_worker_advancing_at_half_a_cpu_per_cpu_is_healthy() {
    local(async {
        let rig = rig();
        let handle = rig.start(spec(Regime::R5, &[0], secs(60))).await.unwrap();
        burn(&rig.fake.process(0), 1);
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.failure, None);
        assert_eq!(run.finished.result.ran, secs(60));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn cancelling_ends_the_trial_with_a_cancelled_error_and_a_confirmed_teardown() {
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
}

#[tokio::test(start_paused = true)]
async fn r4_load_steps_signal_the_group_and_count_stops_and_continues() {
    local(async {
        let rig = rig();
        let mut trial = spec(Regime::R4, &[0], Duration::from_secs(1));
        trial.workload = togi_machine::pick_workload(Regime::R4, 0).clone();
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
}

#[tokio::test(start_paused = true)]
async fn r7_runs_one_instance_per_core_and_progress_names_the_core() {
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
}

#[tokio::test(start_paused = true)]
async fn a_thread_on_a_foreign_cpu_is_an_escape_and_ends_the_trial() {
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
}

#[tokio::test(start_paused = true)]
async fn a_scope_that_is_never_entered_makes_the_trial_inconclusive_not_failed() {
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
}

#[tokio::test(start_paused = true)]
async fn a_lost_sample_sticks_through_the_whole_trial() {
    local(async {
        let rig = rig();
        let handle = rig.start(spec(Regime::R1, &[0], secs(5))).await.unwrap();
        let process = rig.fake.process(0);
        *process.usage.borrow_mut() = Err(ProcError::Observation("boom".into()));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        *process.usage.borrow_mut() = Ok(crate::procfs::Usage {
            cpu_time: secs(1),
            cpu: 0,
        });
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.ran, secs(5));
        assert_eq!(
            run.finished.result.inconclusive.as_deref(),
            Some("core 00 usage sampling lost: boom")
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_disappearing_process_is_not_a_sampling_loss() {
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
}

#[tokio::test(start_paused = true)]
async fn a_partial_thread_listing_still_proves_an_escape_despite_other_failures() {
    local(async {
        let rig = rig();
        let handle = rig.start(spec(Regime::R1, &[0], secs(30))).await.unwrap();
        *rig.fake.process(0).threads.borrow_mut() = Ok(Threads {
            threads: vec![Thread { tid: 9, cpu: 6 }],
            errors: vec![ProcError::Observation("permission denied".into())],
        });
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.escaped, [CpuId(6)]);
        assert_eq!(run.finished.result.inconclusive, None);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn backend_failures_survive_thread_read_loss() {
    local(async {
        let rig = rig();
        rig.fake.on_launch(printing("COMPUTE ERROR\n"));
        let handle = rig.start(spec(Regime::R1, &[0], secs(30))).await.unwrap();
        *rig.fake.process(0).threads.borrow_mut() = Err(ProcError::Observation("EACCES".into()));
        let run = finish(&handle).await;
        assert_eq!(
            run.finished.result.failure.map(|f| f.signal),
            Some(Signal::ComputationError)
        );
        assert_eq!(run.finished.result.inconclusive, None);
        let _ = TestBackend::new("x").classify("ok");
    })
    .await;
}
