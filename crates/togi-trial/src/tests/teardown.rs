//! Teardown, rollback and containment on the fake OS under paused time.

use std::time::Duration;

use test_case::test_case;
use togi_machine::{CancellationToken, Regime, Signal};

use super::harness::{core, crate_error, finish, local, printing, rig, rig_edit, rig_with, spec};
use crate::error::{CleanupFailure, Error};
use crate::id::ScopeName;
use crate::os::fake::{Launch, ProcCtx, Reply, script};
use crate::os::{ExitInfo, GroupSignal, Systemd};
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
        assert!(matches!(partial.error, togi_machine::Error::Containment(_)));
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
                std::fs::write(results, "COMPUTE ERROR").unwrap();
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
