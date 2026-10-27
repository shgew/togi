//! R6 idle bursts, scope readiness before the initial stop, and load-step failures.

use std::time::Duration;

use togi_machine::{CancellationToken, Regime};

use super::harness::{Ticks, crate_error, finish, local, rig, rig_with, running_at, spec, ticking};
use crate::error::Error;
use crate::os::ExitInfo;
use crate::os::GroupSignal;
use crate::os::fake::{Delivery, Launch, ProcCtx, SignalErrorKind, script};
use crate::testing::TestBackend;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// A worker that prints a line whenever it is continued.
fn prints_when_continued(_: usize, _: &crate::launch::LaunchCommand) -> Launch {
    Launch::Run(script(|mut ctx: ProcCtx| async move {
        while let Some(delivery) = ctx.signals.recv().await {
            match delivery {
                Delivery::Group(GroupSignal::Cont) => ctx.print("ok burst\n").await,
                Delivery::Group(GroupSignal::Term | GroupSignal::Kill) | Delivery::Scope(_) => {
                    ctx.exit(ExitInfo::Signal(9));
                    return;
                }
                Delivery::Group(GroupSignal::Stop) => {}
            }
        }
    }))
}

#[test]
fn r6_stops_every_core_before_timing_then_bursts_one_core_at_a_time() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(prints_when_continued);
            let handle = rig
                .start(spec(Regime::R6, &[2, 0, 1], secs(20)))
                .await
                .unwrap();
            let t0 = tokio::time::Instant::now();
            for index in 0..3 {
                assert_eq!(
                    rig.fake.process(index).signals.borrow()[..],
                    [GroupSignal::Stop]
                );
            }
            let run = finish(&handle).await;
            let mut timeline: Vec<(Duration, usize, GroupSignal)> = Vec::new();
            for index in 0..3 {
                for (at, signal) in rig.fake.process(index).signal_times.borrow().iter().skip(1) {
                    timeline.push((at.duration_since(t0), index, *signal));
                }
            }
            timeline.sort_by_key(|(at, index, _)| (*at, *index));
            let bursts: Vec<_> = timeline
                .iter()
                .filter(|(at, _, _)| *at < secs(20))
                .map(|(at, index, signal)| (at.as_millis(), *index, *signal))
                .collect();
            assert_eq!(
                bursts,
                [
                    (10_000, 1, GroupSignal::Cont),
                    (10_100, 1, GroupSignal::Stop),
                    (12_000, 2, GroupSignal::Cont),
                    (12_100, 2, GroupSignal::Stop),
                    (14_000, 0, GroupSignal::Cont),
                    (14_100, 0, GroupSignal::Stop),
                    (16_000, 1, GroupSignal::Cont),
                    (16_100, 1, GroupSignal::Stop),
                    (18_000, 2, GroupSignal::Cont),
                    (18_100, 2, GroupSignal::Stop),
                ]
            );
            assert_eq!(
                (run.finished.result.stops, run.finished.result.conts),
                (8, 5)
            );
            let progress = run.progress();
            assert_eq!(
                progress.first().copied(),
                Some("togi controller CPUs 3,35 while the trial loads 2,0,1; idle CO-0 host core 3 at CO 0, uncapped; 0 threads")
            );
            assert_eq!(
                progress.get(progress.len() - 2).copied(),
                Some("togi's own threads run on their own CPUs again: 0 threads")
            );
            assert_eq!(
                progress.get(1).copied(),
                Some("first half idle, then 100ms bursts every 2s, one core at a time")
            );
            assert_eq!(
                progress.last().copied(),
                Some("bursts: 5 continues, 8 stops")
            );
            let bursts_seen = progress.iter().filter(|p| p.ends_with("ok burst")).count();
            assert_eq!(
                bursts_seen,
                5 + 3,
                "five bursts, then teardown continues each worker once"
            );
            assert_eq!(run.finished.result.failure, None);
        })
        .await;
    });
}

#[test]
fn r6_waits_for_the_scope_before_the_initial_stop() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.set_ready_default(false);
            let helper = {
                let fake = rig.fake.clone();
                tokio::task::spawn_local(async move {
                    while fake.process_count() == 0 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    fake.process(0).ready.set(true);
                })
            };
            let begun = tokio::time::Instant::now();
            let handle = rig.start(spec(Regime::R6, &[0], secs(20))).await.unwrap();
            helper.await.unwrap();
            let (stopped_at, signal) = rig.fake.process(0).signal_times.borrow()[0];
            assert_eq!(signal, GroupSignal::Stop);
            assert!(stopped_at.duration_since(begun) >= Duration::from_millis(100));
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.inconclusive, None);
        })
        .await;
    });
}

#[test]
fn r6_stops_a_worker_that_never_entered_its_scope_after_the_grace_and_goes_on() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let timing = Timing {
                stall_grace: Duration::from_millis(500),
                ..Timing::default()
            };
            let rig = rig_with(TestBackend::new("/bin/true"), timing);
            rig.fake.set_ready_default(false);
            let begun = tokio::time::Instant::now();
            let handle = rig.start(spec(Regime::R6, &[0], secs(4))).await.unwrap();
            let (stopped_at, signal) = rig.fake.process(0).signal_times.borrow()[0];
            assert_eq!(signal, GroupSignal::Stop);
            assert!(stopped_at.duration_since(begun) >= Duration::from_millis(500));
            let run = finish(&handle).await;
            assert!(
                run.finished
                    .result
                    .inconclusive
                    .as_deref()
                    .is_some_and(|reason| reason.contains("never entered scope"))
            );
        })
        .await;
    });
}

#[test]
fn cancelling_during_the_scope_wait_still_stops_the_worker_then_rolls_back() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.set_ready_default(false);
            let cancel = CancellationToken::new();
            let trigger = cancel.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                trigger.cancel();
            });
            let partial = rig
                .start_with(&cancel, spec(Regime::R6, &[0], secs(20)))
                .await
                .err()
                .unwrap();
            assert!(matches!(partial.error, togi_machine::Error::Cancelled));
            assert!(partial.value.is_some());
            assert_eq!(rig.fake.process(0).signals.borrow()[0], GroupSignal::Stop);
            assert!(rig.fake.process(0).reaped.get());
        })
        .await;
    });
}

#[test]
fn a_failed_initial_stop_rolls_the_start_back_and_names_the_signal() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake
                .set_signal_error_default(Some(SignalErrorKind::Failed));
            let partial = rig
                .start(spec(Regime::R6, &[0], secs(20)))
                .await
                .err()
                .unwrap();
            assert!(
                partial.value.is_none(),
                "the group kill failed too, so nothing is confirmed"
            );
            assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
        })
        .await;
    });
}

#[test]
fn a_failed_scheduled_stop_ends_the_trial_and_contains_the_workload() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R4, &[0], secs(5))).await.unwrap();
            *rig.fake.process(0).signal_error.borrow_mut() = Some(SignalErrorKind::Failed);
            let run = finish(&handle).await;
            assert!(matches!(
                &run.finished.errors[..],
                [Error::Signal {
                    signal: "SIGSTOP",
                    ..
                }]
            ));
            let containment = run.finished.teardown.unwrap_err();
            assert!(matches!(
                containment.failures().first(),
                Some(crate::CleanupFailure::SignalGroup {
                    signal: "SIGKILL",
                    ..
                })
            ));
            assert!(run.finished.result.ran < secs(1));
            let _ = crate_error;
        })
        .await;
    });
}

/// The signals one fake process saw, with the instants they arrived.
fn timeline(rig: &super::harness::Rig, index: usize) -> Vec<(tokio::time::Instant, GroupSignal)> {
    rig.fake.process(index).signal_times.borrow().clone()
}

/// Every line the autonomous workers wrote came while its process was not stopped.
fn assert_quiet_while_stopped(rig: &super::harness::Rig, ticks: &Ticks) {
    let ticks = ticks.borrow();
    assert!(
        !ticks.is_empty(),
        "the workers produced output while running"
    );
    for (index, at) in ticks.iter() {
        assert!(
            running_at(&timeline(rig, *index), *at),
            "worker {index} wrote while stopped, at {at:?}"
        );
    }
}

/// One write per 33 ms while running: three in each 100 ms burst.
const TICK: Duration = Duration::from_millis(33);

#[test]
fn r6_workers_that_write_on_their_own_are_silent_while_stopped_and_write_in_bursts() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let ticks = Ticks::default();
            rig.fake.on_launch(ticking(TICK, &ticks));
            let handle = rig
                .start(spec(Regime::R6, &[2, 0, 1], secs(20)))
                .await
                .unwrap();
            let t0 = tokio::time::Instant::now();
            let run = finish(&handle).await;
            assert_quiet_while_stopped(&rig, &ticks);
            let first_burst = t0 + secs(10);
            {
                let ticks = ticks.borrow();
                assert!(
                    ticks.iter().all(|(_, at)| *at < t0 || *at >= first_burst),
                    "the idle half is silent"
                );
                // Five bursts of 100 ms at one write per 33 ms, then the teardown's own continue.
                for (burst, index) in [(10, 1), (12, 2), (14, 0), (16, 1), (18, 2)] {
                    let from = t0 + secs(burst);
                    assert!(
                        ticks
                            .iter()
                            .any(|(i, at)| *i == index && *at >= from && *at <= from + secs(1)),
                        "worker {index} wrote during its burst at {burst}s"
                    );
                }
            }
            assert_eq!(run.finished.result.failure, None);
        })
        .await;
    });
}

#[test]
fn r4_workers_that_write_on_their_own_are_silent_while_stopped() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let ticks = Ticks::default();
            rig.fake.on_launch(ticking(TICK, &ticks));
            let handle = rig.start(spec(Regime::R4, &[0], secs(5))).await.unwrap();
            let run = finish(&handle).await;
            assert!(
                run.finished.result.stops > 0,
                "the schedule stops the worker at least once"
            );
            assert_quiet_while_stopped(&rig, &ticks);
            assert_eq!(run.finished.result.failure, None);
        })
        .await;
    });
}

/// A refused `SIGSTOP` before any other signal: the start rolls back, the teardown's signals
/// succeed, so the workload is proved stopped and the original signal error is the one reported.
#[test]
fn a_refused_initial_stop_with_confirmed_cleanup_keeps_the_signal_error_and_the_proof() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake
                .fail_signal_once(GroupSignal::Stop, SignalErrorKind::Failed);
            let partial = rig
                .start(spec(Regime::R6, &[0], secs(20)))
                .await
                .err()
                .unwrap();
            assert!(
                partial.value.is_some(),
                "the cleanup was confirmed, so the proof is returned"
            );
            assert!(matches!(
                crate_error(&partial.error),
                Some(Error::Signal {
                    signal: "SIGSTOP",
                    ..
                })
            ));
            let process = rig.fake.process(0);
            assert!(
                process.exited() && process.reaped.get(),
                "the workload is reaped"
            );
        })
        .await;
    });
}

/// A refused scheduled `SIGSTOP`, then a teardown whose signals succeed: the trial ends with the
/// original signal error, containment is confirmed and the workload is reaped.
async fn a_refused_scheduled_stop_is_cleaned_up(regime: Regime, duration: Duration) {
    let rig = rig();
    let handle = rig.start(spec(regime, &[0], duration)).await.unwrap();
    rig.fake
        .fail_signal_once(GroupSignal::Stop, SignalErrorKind::Failed);
    let run = finish(&handle).await;
    assert!(matches!(
        &run.finished.errors[..],
        [Error::Signal {
            signal: "SIGSTOP",
            ..
        }]
    ));
    assert!(
        run.finished.teardown.is_ok(),
        "the cleanup was confirmed: {:?}",
        run.finished.teardown.as_ref().err()
    );
    let process = rig.fake.process(0);
    assert!(
        process.exited() && process.reaped.get(),
        "the workload is reaped"
    );
    assert!(run.finished.result.ran < duration, "the trial ended early");
}

#[test]
fn a_refused_scheduled_stop_with_confirmed_cleanup_keeps_the_signal_error_in_r4() {
    crate::testing::runtime(true).block_on(async move {
        local(a_refused_scheduled_stop_is_cleaned_up(Regime::R4, secs(5))).await;
    });
}

#[test]
fn a_refused_burst_stop_with_confirmed_cleanup_keeps_the_signal_error_in_r6() {
    crate::testing::runtime(true).block_on(async move {
        local(a_refused_scheduled_stop_is_cleaned_up(Regime::R6, secs(20))).await;
    });
}
