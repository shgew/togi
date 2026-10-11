//! Teardown, rollback and containment on the fake OS under paused time.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use test_case::test_case;
use togi_backend::{Backend, Line, Plan, Ready};
use togi_machine::{CancellationToken, CpuId, Regime, Signal, Workload};

use super::harness::{core, crate_error, finish, local, printing, rig, rig_edit, rig_with, spec};
use crate::error::{CleanupFailure, Error};
use crate::id::ScopeName;
use crate::os::fake::{Launch, ProcCtx, Reply, ScanFault, script};
use crate::os::{ExitInfo, GroupSignal, Systemd};
use crate::runner::Backends;
use crate::testing::TestBackend;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn kills(calls: &[(Systemd, tokio::time::Instant)]) -> Vec<String> {
    let mut scopes: Vec<String> = calls
        .iter()
        .filter_map(|(call, _)| match call {
            Systemd::Kill { scope, signal } if *signal == crate::os::KillSignal::Kill => {
                Some(scope.to_string())
            }
            _ => None,
        })
        .collect();
    scopes.sort();
    scopes
}

/// A process that never exits and holds its pipes: nothing can confirm it stopped.
fn immortal(_: usize, _: &crate::launch::LaunchCommand) -> Launch {
    Launch::Run(script(|ctx: ProcCtx| async move {
        let _keep = ctx;
        std::future::pending::<()>().await;
    }))
}

/// The failure list a containment error at the seam carries, also when later errors joined it.
fn containment_of(error: &togi_machine::Error) -> &crate::Containment {
    let togi_machine::Error::Containment(source) = error else {
        panic!("not containment: {error:?}");
    };
    if let Some(containment) = source.downcast_ref::<std::sync::Arc<crate::Containment>>() {
        return containment;
    }
    let Some(Error::Joined(joined)) = source.downcast_ref::<Error>() else {
        panic!("not a failure list: {source:?}");
    };
    let Some(Error::Containment(containment)) = joined.errors().first() else {
        panic!("the containment is not first: {joined:?}");
    };
    containment
}

#[test_case(1; "one instance")]
#[test_case(8; "eight instances")]
#[test_case(16; "sixteen instances")]
#[tokio::test(start_paused = true)]
async fn teardown_shares_one_fifteen_second_budget_across_instances(count: u8) {
    local(async {
        let rig = rig();
        rig.fake.on_launch(immortal);
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Hang),
            _ => None,
        });
        let cores: Vec<u8> = (0..count).collect();
        let cancel = CancellationToken::new();
        let regime = if count == 1 { Regime::R1 } else { Regime::R7 };
        let handle = rig
            .start_with(&cancel, spec(regime, &cores, secs(600)))
            .await
            .unwrap();
        tokio::time::sleep(secs(2)).await;
        let begun = tokio::time::Instant::now();
        cancel.cancel();
        let run = finish(&handle).await;
        let elapsed = begun.elapsed();
        assert!(
            elapsed >= secs(15) && elapsed <= secs(15) + Duration::from_millis(50),
            "{elapsed:?}"
        );
        let containment = run.finished.teardown.unwrap_err();
        assert!(matches!(
            containment.failures().first(),
            Some(CleanupFailure::KillScope { .. })
        ));
        for index in 0..usize::from(count) {
            assert_eq!(
                rig.fake.process(index).signals.borrow()[..],
                [GroupSignal::Cont, GroupSignal::Term, GroupSignal::Kill]
            );
        }
        assert_eq!(kills(&rig.fake.systemd_calls()).len(), usize::from(count));
        let slot_end = begun + secs(5);
        assert!(
            rig.fake
                .systemd_calls()
                .iter()
                .all(|(_, at)| *at <= slot_end)
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_timed_out_scope_kill_is_never_read_as_a_missing_scope() {
    local(async {
        let rig = rig();
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Hang),
            _ => None,
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let run = finish(&handle).await;
        assert!(run.finished.teardown.is_err());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_completed_not_loaded_answer_counts_as_a_missing_scope() {
    local(async {
        let rig = rig();
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Output {
                success: false,
                text: "Failed to kill unit: Unit togi-trial-t1.scope not loaded.".into(),
            }),
            _ => None,
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let run = finish(&handle).await;
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn another_failure_text_is_a_failed_kill() {
    local(async {
        let rig = rig();
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Output {
                success: false,
                text: "Access denied".into(),
            }),
            _ => None,
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let run = finish(&handle).await;
        assert!(matches!(
            run.finished.teardown.unwrap_err().failures(),
            [CleanupFailure::KillScope { .. }]
        ));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_partial_start_that_cannot_confirm_is_containment_with_both_scopes_targeted() {
    local(async {
        let rig = rig();
        rig.fake.on_launch(|n, command| {
            if n == 0 {
                immortal(n, command)
            } else {
                Launch::Fail(std::io::Error::other("systemd-run missing"))
            }
        });
        let begun = tokio::time::Instant::now();
        let partial = rig
            .start(spec(Regime::R7, &[0, 1], secs(60)))
            .await
            .err()
            .unwrap();
        assert!(begun.elapsed() <= secs(15));
        assert!(partial.value.is_none());
        let failures = containment_of(&partial.error).failures();
        let [
            CleanupFailure::Start(cause),
            CleanupFailure::Reap { instance, .. },
            CleanupFailure::DrainDeadline,
            CleanupFailure::ScopeMembers { scope },
        ] = failures
        else {
            panic!("{failures:?}");
        };
        assert!(
            matches!(
                &**cause,
                Error::Launch { instance, source, .. }
                    if instance == "c01" && source.to_string() == "systemd-run missing"
            ),
            "{cause:?}"
        );
        assert_eq!(
            (instance.as_str(), scope.as_str()),
            ("c00", "togi-trial-t1-c00")
        );
        assert_eq!(
            kills(&rig.fake.systemd_calls()),
            ["togi-trial-t1-c00", "togi-trial-t1-c01"]
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_partial_start_that_confirms_keeps_its_cause_without_containment() {
    local(async {
        let rig = rig();
        rig.fake.on_launch(|n, command| {
            if n == 0 {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.until_killed().await;
                }))
            } else {
                let _ = command;
                Launch::Fail(std::io::Error::other("systemd-run missing"))
            }
        });
        let partial = rig
            .start(spec(Regime::R7, &[0, 1], secs(60)))
            .await
            .err()
            .unwrap();
        assert!(partial.value.is_some());
        assert!(matches!(
            crate_error(&partial.error),
            Some(Error::Launch { instance, .. }) if instance == "c01"
        ));
        assert_eq!(
            kills(&rig.fake.systemd_calls()),
            ["togi-trial-t1-c00", "togi-trial-t1-c01"]
        );
        assert!(rig.fake.process(0).reaped.get());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn nothing_launches_when_preparation_fails() {
    local(async {
        let mut failing = TestBackend::new("/bin/true");
        failing.fail_plan = true;
        let rig = rig_with(failing, Timing::default());
        let partial = rig
            .start(spec(Regime::R1, &[0], secs(5)))
            .await
            .err()
            .unwrap();
        assert!(partial.value.is_some());
        assert!(matches!(
            crate_error(&partial.error),
            Some(Error::Prepare(_))
        ));
        assert!(rig.fake.launches().is_empty());

        let rig = rig_with(TestBackend::new(""), Timing::default());
        let partial = rig
            .start(spec(Regime::R1, &[0], secs(5)))
            .await
            .err()
            .unwrap();
        assert!(partial.value.is_some());
        assert!(rig.fake.launches().is_empty());

        let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
            let obstacle = env.dir.parent().unwrap().join("obstacle");
            std::fs::write(&obstacle, "x").unwrap();
            env.dir = obstacle.join("trials");
        });
        let partial = rig
            .start(spec(Regime::R1, &[0], secs(5)))
            .await
            .err()
            .unwrap();
        assert!(partial.value.is_some());
        assert!(rig.fake.launches().is_empty());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_invalid_spec_or_missing_backend_or_identity_never_touches_the_disk() {
    local(async {
        let rig = rig();
        let mut bad = spec(Regime::R1, &[0], secs(5));
        bad.id = "../escape".into();
        let partial = rig.start(bad).await.err().unwrap();
        assert!(partial.value.is_some());
        assert!(!rig.dir.path().join("trials").exists());

        let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
            env.backends = crate::runner::Backends::new();
        });
        let partial = rig
            .start(spec(Regime::R1, &[0], secs(5)))
            .await
            .err()
            .unwrap();
        assert!(matches!(partial.error, togi_machine::Error::BackendMissing));
        assert!(partial.value.is_some());

        let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
            env.identity = None;
        });
        let partial = rig
            .start(spec(Regime::R1, &[0], secs(5)))
            .await
            .err()
            .unwrap();
        assert!(matches!(
            crate_error(&partial.error),
            Some(Error::Identity(crate::IdentityError::NotConfigured))
        ));
        assert!(!rig.dir.path().join("trials").exists());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn final_unterminated_pipe_output_survives_confirmed_and_unconfirmed_cleanup() {
    for hang_scope in [false, true] {
        local(async {
            let rig = rig();
            rig.fake.on_launch(|_, _| {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.until_killed_ignoring_term().await;
                    ctx.print("COMPUTE ERROR").await;
                }))
            });
            if hang_scope {
                rig.fake.on_systemd(|call| match call {
                    Systemd::Kill { .. } => Some(Reply::Hang),
                    _ => None,
                });
            }
            let cancel = CancellationToken::new();
            let handle = rig
                .start_with(&cancel, spec(Regime::R1, &[4], secs(60)))
                .await
                .unwrap();
            cancel.cancel();
            let run = finish(&handle).await;
            let failure = run.finished.result.failure.unwrap();
            assert_eq!(
                (failure.signal, failure.core),
                (Signal::ComputationError, core(4))
            );
            assert_eq!(run.finished.teardown.is_err(), hang_scope);
        })
        .await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_pipe_read_error_is_kept_as_containment_and_evidence() {
    local(async {
        let rig = rig();
        let handle = rig.start(spec(Regime::R1, &[0], secs(60))).await.unwrap();
        tokio::time::sleep(secs(1)).await;
        rig.fake.process(0).fail_stdout();
        let run = finish(&handle).await;
        let containment = run.finished.teardown.unwrap_err();
        assert!(matches!(
            containment.failures().first(),
            Some(CleanupFailure::ReadPipe {
                stream: "stdout.log",
                ..
            })
        ));
        assert!(matches!(&run.finished.errors[..], [Error::Output(_)]));
        assert!(run.finished.result.inconclusive.is_some());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_process_that_ignores_term_gets_killed_after_the_grace() {
    local(async {
        let rig = rig();
        rig.fake.on_launch(|_, _| {
            Launch::Run(script(|mut ctx: ProcCtx| async move {
                ctx.until_killed_ignoring_term().await;
            }))
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let begun = tokio::time::Instant::now();
        let run = finish(&handle).await;
        assert!(run.finished.teardown.is_ok());
        assert!(begun.elapsed() >= secs(2), "grace is waited out");
        let times = rig.fake.process(0).signal_times.borrow().clone();
        assert_eq!(times.len(), 3);
        assert_eq!(times[2].1, GroupSignal::Kill);
        let _ = ExitInfo::Code(0);
        let _ = ScopeName::from_unit("togi-trial-x.scope");
        let _ = printing("");
    })
    .await;
}

/// The leader exits when asked, but a detached descendant keeps the pipes open until its scope dies.
fn leaves_a_pipe_holder(_: usize, _: &crate::launch::LaunchCommand) -> Launch {
    Launch::Run(script(|mut ctx: ProcCtx| async move {
        ctx.until_killed().await;
        ctx.until_scope_killed().await;
    }))
}

#[tokio::test(start_paused = true)]
async fn a_detached_pipe_holder_ends_with_the_scope_kill_and_teardown_confirms() {
    local(async {
        let rig = rig();
        rig.fake.on_launch(leaves_a_pipe_holder);
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let run = finish(&handle).await;
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_detached_pipe_holder_beyond_a_failed_scope_kill_leaves_the_drain_unconfirmed() {
    local(async {
        let rig = rig();
        rig.fake.on_launch(leaves_a_pipe_holder);
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Hang),
            _ => None,
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let begun = tokio::time::Instant::now();
        let run = finish(&handle).await;
        assert!(begun.elapsed() <= secs(15) + Duration::from_millis(50));
        let containment = run.finished.teardown.unwrap_err();
        assert!(
            containment
                .failures()
                .iter()
                .any(|failure| matches!(failure, CleanupFailure::DrainDeadline))
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_pipe_setup_failure_rolls_back_as_unconfirmed_output() {
    local(async {
        let rig = rig();
        rig.fake.fail_pipes();
        let partial = rig
            .start(spec(Regime::R1, &[0], secs(60)))
            .await
            .err()
            .unwrap();
        assert!(
            partial.value.is_none(),
            "unread output is never a confirmed drain"
        );
        assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
        let process = rig.fake.process(0);
        assert!(
            process.reaped.get(),
            "the launched child was still contained and reaped"
        );
        assert_eq!(kills(&rig.fake.systemd_calls()), ["togi-trial-t1"]);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_launcher_whose_exit_watch_failed_is_still_killed_and_reaped_by_the_rollback() {
    local(async {
        let rig = rig();
        rig.fake.on_launch(|_, _| {
            Launch::FailRetained(
                script(|mut ctx: ProcCtx| async move { ctx.until_killed().await }),
                std::io::Error::other("pidfd_open failed"),
            )
        });
        let begun = tokio::time::Instant::now();
        let partial = rig
            .start(spec(Regime::R1, &[0], secs(60)))
            .await
            .err()
            .unwrap();
        assert!(begun.elapsed() <= secs(15));
        assert!(
            partial.value.is_some(),
            "the retained child was confirmed stopped"
        );
        assert!(matches!(
            crate_error(&partial.error),
            Some(Error::Launch { .. })
        ));
        assert!(rig.fake.process(0).reaped.get());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_acknowledged_scope_kill_with_a_surviving_member_is_containment() {
    local(async {
        let mut backend = TestBackend::new("/bin/true");
        backend.watched = vec!["results.txt"];
        let rig = rig_with(backend, Timing::default());
        rig.fake.on_launch(|_, command| {
            let results = command.cwd.join("results.txt");
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                ctx.write_file(results, "COMPUTE ERROR").unwrap();
                ctx.until_killed().await;
            }))
        });
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
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let begun = tokio::time::Instant::now();
        let run = finish(&handle).await;
        assert!(begun.elapsed() <= secs(15) + Duration::from_millis(50));
        let containment = run.finished.teardown.unwrap_err();
        assert!(matches!(
            containment.failures(),
            [CleanupFailure::ScopeMembers { scope }] if scope == "togi-trial-t1"
        ));
        assert_eq!(
            run.finished.result.failure, None,
            "a watched partial is not classified while its writers may live"
        );
        assert!(survivor.alive.get());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_member_that_exits_after_the_kill_is_waited_for_and_then_confirmed() {
    local(async {
        let rig = rig();
        let survivor = rig.fake.add_recovered(crate::procfs::ScopeProcess {
            scope: ScopeName::from_unit("togi-trial-t1.scope").unwrap(),
            pid: 9001,
            starttime: 1,
        });
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Output {
                success: true,
                text: String::new(),
            }),
            _ => None,
        });
        let alive = survivor.alive.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(secs(5)).await;
            alive.set(false);
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let run = finish(&handle).await;
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

fn watching_rig() -> super::harness::Rig {
    let mut backend = TestBackend::new("/bin/true");
    backend.watched = vec!["results.txt"];
    let rig = rig_with(backend, Timing::default());
    rig.fake.on_launch(|_, command| {
        let results = command.cwd.join("results.txt");
        Launch::Run(script(move |mut ctx: ProcCtx| async move {
            ctx.write_file(results, "COMPUTE ERROR").unwrap();
            ctx.until_killed().await;
        }))
    });
    rig
}

#[test_case(false, ScanFault::Fail, "scan failed"; "a failed scan")]
#[test_case(true, ScanFault::Fail, "scan failed"; "a failed scan after a failed kill")]
#[test_case(false, ScanFault::Late, "after the teardown deadline"; "an empty scan after the deadline")]
#[test_case(true, ScanFault::Late, "after the teardown deadline"; "an empty scan after the deadline and a failed kill")]
#[tokio::test(start_paused = true)]
async fn an_unverifiable_scope_scan_withholds_the_proof_and_the_watched_partial(
    kill_fails: bool,
    fault: ScanFault,
    reason: &str,
) {
    local(async {
        let rig = watching_rig();
        rig.fake.fault_scans(fault);
        if kill_fails {
            rig.fake.on_systemd(|call| match call {
                Systemd::Kill { .. } => Some(Reply::Output {
                    success: false,
                    text: "Access denied".into(),
                }),
                _ => None,
            });
        }
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let begun = tokio::time::Instant::now();
        let run = finish(&handle).await;
        assert!(begun.elapsed() <= secs(15) + Duration::from_millis(50));
        let containment = run.finished.teardown.unwrap_err();
        let failures = containment.failures();
        let observed = if kill_fails {
            let [CleanupFailure::KillScope { source, .. }, observed] = failures else {
                panic!("{failures:?}");
            };
            assert!(source.to_string().contains("Access denied"), "{source}");
            observed
        } else {
            let [observed] = failures else {
                panic!("{failures:?}");
            };
            observed
        };
        let CleanupFailure::Observe { what, source } = observed else {
            panic!("{observed:?}");
        };
        assert_eq!(*what, "the trial scope members");
        assert!(source.to_string().contains(reason), "{source}");
        assert_eq!(
            run.finished.result.failure, None,
            "a watched partial is not classified while the scope is unverified"
        );
    })
    .await;
}

#[test_case(1; "one scope")]
#[test_case(2; "two scopes")]
#[tokio::test(start_paused = true)]
async fn a_scope_kill_that_outlives_its_deadline_is_cut_off_at_the_shared_slot(count: u8) {
    local(async {
        let rig = rig();
        rig.fake.on_launch(|_, _| {
            Launch::Run(script(|mut ctx: ProcCtx| async move {
                ctx.until_killed_ignoring_term().await;
            }))
        });
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Stall),
            _ => None,
        });
        let cores: Vec<u8> = (0..count).collect();
        let regime = if count == 1 { Regime::R1 } else { Regime::R7 };
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(regime, &cores, secs(600)))
            .await
            .unwrap();
        let begun = tokio::time::Instant::now();
        cancel.cancel();
        let run = finish(&handle).await;
        let timing = Timing::default();
        let slot_start = begun + timing.stop_grace;
        let slot_end = slot_start + timing.kill_slot;
        let calls = rig.fake.systemd_calls();
        assert_eq!(kills(&calls).len(), usize::from(count));
        assert!(calls.iter().all(|(_, at)| *at == slot_start), "{calls:?}");
        for index in 0..usize::from(count) {
            let times = rig.fake.process(index).signal_times.borrow().clone();
            let (kill_at, signal) = *times.last().unwrap();
            assert_eq!(signal, GroupSignal::Kill);
            assert_eq!(kill_at, slot_end, "the group kill follows the slot");
        }
        let containment = run.finished.teardown.unwrap_err();
        let failures = containment.failures();
        assert_eq!(failures.len(), usize::from(count), "{failures:?}");
        for failure in failures {
            let CleanupFailure::KillScope { source, .. } = failure else {
                panic!("{failure:?}");
            };
            assert!(
                source
                    .downcast_ref::<tokio::time::error::Elapsed>()
                    .is_some(),
                "{source}"
            );
        }
        assert!(
            begun.elapsed() <= timing.stop_grace + timing.kill_slot + Duration::from_millis(50)
        );
    })
    .await;
}

fn boom_backend(watched: bool) -> TestBackend {
    let mut backend = TestBackend::new("/bin/true");
    backend.panic_on = Some("BOOM");
    if watched {
        backend.watched = vec!["results.txt"];
    }
    backend
}

#[test_case(false; "final pipe output")]
#[test_case(true; "a watched partial")]
#[tokio::test(start_paused = true)]
async fn a_classifier_panic_during_teardown_is_reported_and_teardown_stays_bounded(watched: bool) {
    local(async {
        let rig = rig_with(boom_backend(watched), Timing::default());
        rig.fake.on_launch(move |_, command| {
            let results = command.cwd.join("results.txt");
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                if watched {
                    ctx.write_file(results, "BOOM").unwrap();
                    ctx.until_killed().await;
                } else {
                    ctx.until_killed_ignoring_term().await;
                    ctx.print("BOOM\n").await;
                }
            }))
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(600)))
            .await
            .unwrap();
        cancel.cancel();
        let begun = tokio::time::Instant::now();
        let run = tokio::time::timeout(secs(60), finish(&handle))
            .await
            .expect("the supervisor published an end after the panic");
        assert!(begun.elapsed() <= secs(15) + Duration::from_millis(50));
        assert!(run.finished.teardown.is_ok(), "the stop is still proven");
        assert!(rig.fake.process(0).reaped.get());
        assert!(
            matches!(
                &run.finished.errors[..],
                [Error::Cancelled, Error::Panicked { what }] if what.contains("the classifier met BOOM")
            ),
            "{:?}",
            run.finished.errors
        );
        assert!(
            run.finished.result.inconclusive.is_some(),
            "a trial whose classifier panicked cannot pass: {:?}",
            run.finished.result
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_classifier_panic_during_rollback_is_joined_after_the_launch_cause() {
    local(async {
        let rig = rig_with(boom_backend(false), Timing::default());
        rig.fake.on_launch(|n, _| {
            if n == 0 {
                Launch::Run(script(|mut ctx: ProcCtx| async move {
                    ctx.until_killed_ignoring_term().await;
                    ctx.print("BOOM\n").await;
                }))
            } else {
                Launch::Fail(std::io::Error::other("systemd-run missing"))
            }
        });
        let begun = tokio::time::Instant::now();
        let partial = rig
            .start(spec(Regime::R7, &[0, 1], secs(60)))
            .await
            .err()
            .unwrap();
        assert!(begun.elapsed() <= secs(15) + Duration::from_millis(50));
        assert!(partial.value.is_some(), "the stop is still proven");
        let Some(Error::Joined(joined)) = crate_error(&partial.error) else {
            panic!("{:?}", partial.error);
        };
        assert!(
            matches!(
                joined.errors(),
                [Error::Launch { instance, .. }, Error::Panicked { what }]
                    if instance == "c01" && what.contains("the classifier met BOOM")
            ),
            "{joined:?}"
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_watched_append_after_the_last_drain_poll_is_classified_before_the_proof() {
    local(async {
        let rig = watching_rig();
        rig.fake.on_launch(|_, command| {
            let results = command.cwd.join("results.txt");
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                ctx.write_file(results, "COMPUTE").unwrap();
                ctx.until_killed().await;
            }))
        });
        let fake = rig.fake.clone();
        let appended = Arc::new(AtomicUsize::new(0));
        let appends = Arc::clone(&appended);
        rig.fake.on_scan(move || {
            if appends.fetch_add(1, Ordering::SeqCst) == 0 {
                use std::io::Write as _;
                let path = fake.launches()[0].cwd.join("results.txt");
                let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
                file.write_all(b" ERROR").unwrap();
            }
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[3], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let run = finish(&handle).await;
        assert!(appended.load(Ordering::SeqCst) >= 1, "the scan ran");
        let failure = run.finished.result.failure.expect("the late append counts");
        assert_eq!(
            (failure.signal, failure.core),
            (Signal::ComputationError, core(3))
        );
        assert!(run.finished.teardown.is_ok(), "{:?}", run.finished.teardown);
    })
    .await;
}

struct FinalBudget {
    inner: TestBackend,
    until: Arc<std::sync::OnceLock<tokio::time::Instant>>,
    classified: Arc<AtomicUsize>,
}

impl Backend for FinalBudget {
    fn kind(&self) -> togi_machine::Backend {
        self.inner.kind()
    }

    fn check(&self) -> Result<Ready, togi_backend::Error> {
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
        if line == "spend final budget" {
            self.classified.fetch_add(1, Ordering::SeqCst);
            let until = self.until.get().expect("the scan set the bound");
            std::thread::sleep(until.saturating_duration_since(tokio::time::Instant::now()));
        }
        self.inner.classify(line)
    }
}

#[tokio::test]
async fn a_final_partial_consuming_the_budget_cannot_confirm_another_files_old_snapshot() {
    local(async {
        let mut inner = TestBackend::new("/bin/true");
        inner.watched = vec!["results.txt", "second.txt"];
        let until = Arc::new(std::sync::OnceLock::new());
        let classified = Arc::new(AtomicUsize::new(0));
        let backend = FinalBudget {
            inner: inner.clone(),
            until: Arc::clone(&until),
            classified: Arc::clone(&classified),
        };
        let timing = Timing {
            stop_grace: Duration::from_millis(50),
            kill_slot: Duration::from_millis(50),
            drain: Duration::from_millis(500),
            teardown: Duration::from_secs(1),
            ..Timing::default()
        };
        let rig = rig_edit(inner, timing, move |env| {
            env.backends = Backends::new().with(Arc::new(backend));
        });
        rig.fake.on_launch(|_, command| {
            let first = command.cwd.join("results.txt");
            let second = command.cwd.join("second.txt");
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                ctx.write_file(first, "spend final budget").unwrap();
                ctx.write_file(second, "COMPUTE").unwrap();
                ctx.until_killed().await;
            }))
        });
        let fake = rig.fake.clone();
        let scans = Arc::new(AtomicUsize::new(0));
        let scanned = Arc::clone(&scans);
        rig.fake.on_scan(move || {
            if scanned.fetch_add(1, Ordering::SeqCst) == 0 {
                use std::io::Write as _;
                let path = fake.launches()[0].cwd.join("second.txt");
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(path)
                    .unwrap()
                    .write_all(b" ERROR")
                    .unwrap();
                let began = fake.process(0).signal_times.borrow()[0].0;
                until
                    .set(began + timing.teardown + Duration::from_millis(20))
                    .unwrap();
            }
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[3], secs(60)))
            .await
            .unwrap();
        cancel.cancel();
        let run = finish(&handle).await;
        assert_eq!(classified.load(Ordering::SeqCst), 1);
        assert!(scans.load(Ordering::SeqCst) >= 1);
        assert_eq!(run.finished.result.failure, None);
        let containment = run
            .finished
            .teardown
            .expect_err("an unfinished final reread cannot mint a stop proof");
        assert!(matches!(
            containment.failures(),
            [CleanupFailure::DrainDeadline]
        ));
    })
    .await;
}

struct Naps {
    inner: TestBackend,
    nap: Duration,
    classified: Arc<AtomicUsize>,
}

impl Backend for Naps {
    fn kind(&self) -> togi_machine::Backend {
        self.inner.kind()
    }

    fn check(&self) -> Result<Ready, togi_backend::Error> {
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
        self.classified.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.nap);
        self.inner.classify(line)
    }
}

#[test_case(1; "one instance")]
#[test_case(8; "eight instances")]
#[test_case(16; "sixteen instances")]
#[tokio::test]
async fn slow_watched_output_of_many_files_is_bound_by_one_cleanup_budget(count: u8) {
    const LINES: usize = 100;
    local(async {
        let mut inner = TestBackend::new("/bin/true");
        inner.watched = vec!["results.txt", "second.txt"];
        let classified = Arc::new(AtomicUsize::new(0));
        let slow = Naps {
            inner: inner.clone(),
            nap: Duration::from_millis(20),
            classified: Arc::clone(&classified),
        };
        let timing = Timing {
            stop_grace: Duration::from_millis(100),
            kill_slot: Duration::from_millis(100),
            drain: Duration::from_millis(600),
            teardown: Duration::from_secs(1),
            ..Timing::default()
        };
        let rig = rig_edit(inner, timing, move |env| {
            env.backends = Backends::new().with(Arc::new(slow));
        });
        rig.fake.on_launch(|_, command| {
            let first = command.cwd.join("results.txt");
            let second = command.cwd.join("second.txt");
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                let text = format!("{}COMPUTE ERROR", "ok\n".repeat(LINES));
                std::fs::write(first, &text).unwrap();
                std::fs::write(second, &text).unwrap();
                ctx.until_killed().await;
            }))
        });
        let cores: Vec<u8> = (0..count).collect();
        let regime = if count == 1 { Regime::R1 } else { Regime::R7 };
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(regime, &cores, secs(60)))
            .await
            .unwrap();
        let begun = std::time::Instant::now();
        cancel.cancel();
        let run = finish(&handle).await;
        let elapsed = begun.elapsed();
        assert!(
            elapsed <= timing.teardown + Duration::from_millis(500),
            "one absolute budget bounds the drain of every file: {elapsed:?}"
        );
        let containment = run.finished.teardown.unwrap_err();
        assert!(
            containment
                .failures()
                .iter()
                .any(|failure| matches!(failure, CleanupFailure::DrainDeadline)),
            "{:?}",
            containment.failures()
        );
        assert!(
            classified.load(Ordering::SeqCst) < usize::from(count) * 2 * LINES,
            "the unread backlog was not classified in full"
        );
        assert_eq!(
            run.finished.result.failure, None,
            "the unread COMPUTE ERROR is never seen"
        );
    })
    .await;
}
