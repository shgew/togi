//! R6 idle bursts, scope readiness before the initial stop, and load-step failures.

use std::time::Duration;

use togi_machine::{CancellationToken, Regime};

use super::harness::{crate_error, finish, local, rig, rig_with, spec};
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

#[tokio::test(start_paused = true)]
async fn r6_stops_every_core_before_timing_then_bursts_one_core_at_a_time() {
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
}

#[tokio::test(start_paused = true)]
async fn r6_waits_for_the_scope_before_the_initial_stop() {
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
}

#[tokio::test(start_paused = true)]
async fn r6_stops_a_worker_that_never_entered_its_scope_after_the_grace_and_goes_on() {
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
}

#[tokio::test(start_paused = true)]
async fn cancelling_during_the_scope_wait_still_stops_the_worker_then_rolls_back() {
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
}

#[tokio::test(start_paused = true)]
async fn a_failed_initial_stop_rolls_the_start_back_and_names_the_signal() {
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
        assert!(matches!(partial.error, togi_machine::Error::Containment));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_failed_scheduled_stop_ends_the_trial_and_contains_the_workload() {
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
}
