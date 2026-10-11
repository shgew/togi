//! Output framing, the 64 KiB cap, watched files and their evidence under teardown.

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::time::Duration;

use test_case::test_case;
use togi_machine::{CancellationToken, CpuId, Regime, Signal};
use tokio::io::AsyncWriteExt as _;

use super::harness::{finish, local, rig_edit, rig_with, spec};
use crate::error::{CleanupFailure, Error};
use crate::os::fake::{Delivery, Launch, ProcCtx, Reply, SignalErrorKind, script};
use crate::os::{ExitInfo, GroupSignal, Systemd};
use crate::output::LINE_LIMIT;
use crate::procfs::{Thread, Threads};
use crate::testing::TestBackend;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[derive(Clone, Copy, Debug)]
enum Source {
    Stdout,
    Stderr,
    Watched,
}

fn watching() -> TestBackend {
    let mut backend = TestBackend::new("/bin/true");
    backend.watched = vec!["results.txt"];
    backend
}

fn results(command: &crate::launch::LaunchCommand) -> PathBuf {
    command.cwd.join("results.txt")
}

/// A process that emits `bytes` on one source, then lives until killed.
fn emits(
    source: Source,
    bytes: Vec<u8>,
) -> impl FnMut(usize, &crate::launch::LaunchCommand) -> Launch {
    emits_lingering(source, bytes, false)
}

/// Like `emits`, but the process ignores `SIGTERM` when `lingers`, so its pipes stay open.
fn emits_lingering(
    source: Source,
    bytes: Vec<u8>,
    lingers: bool,
) -> impl FnMut(usize, &crate::launch::LaunchCommand) -> Launch {
    move |_, command| {
        let (bytes, file) = (bytes.clone(), results(command));
        Launch::Run(script(move |mut ctx: ProcCtx| async move {
            match source {
                Source::Stdout => {
                    let _ = ctx.stdout.write_all(&bytes).await;
                }
                Source::Stderr => {
                    let _ = ctx.stderr.write_all(&bytes).await;
                }
                Source::Watched => std::fs::write(file, &bytes).unwrap(),
            }
            if lingers {
                ctx.until_killed_ignoring_term().await;
            } else {
                ctx.until_killed().await;
            }
        }))
    }
}

fn log(rig: &super::harness::Rig, name: &str) -> Vec<u8> {
    std::fs::read(rig.dir.path().join("trials/t1/work").join(name)).unwrap()
}

#[test_case(Source::Stdout, "stdout.log"; "stdout")]
#[test_case(Source::Stderr, "stderr.log"; "stderr")]
#[test_case(Source::Watched, "watched output"; "watched file")]
#[tokio::test(start_paused = true)]
async fn an_oversized_line_is_inconclusive_keeps_its_prefix_and_is_not_containment(
    source: Source,
    name: &str,
) {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        rig.fake.on_launch(emits(source, vec![b'x'; 1 << 20]));
        let handle = rig.start(spec(Regime::R1, &[0], secs(60))).await.unwrap();
        let run = finish(&handle).await;
        let result = &run.finished.result;
        assert_eq!(
            result.inconclusive,
            Some(format!(
                "core 00 read {name}: backend output line exceeds 64 KiB"
            ))
        );
        assert_eq!((result.failure, result.escaped.len()), (None, 0));
        assert!(run.finished.teardown.is_ok());
        assert!(run.finished.errors.is_empty());
        match source {
            Source::Stdout => assert_eq!(log(&rig, "stdout.log"), vec![b'x'; LINE_LIMIT]),
            Source::Stderr => assert_eq!(log(&rig, "stderr.log"), vec![b'x'; LINE_LIMIT]),
            Source::Watched => {}
        }
    })
    .await;
}

fn exact_limit_line(terminator: &str) -> Vec<u8> {
    let mut line = b"COMPUTE ERROR".to_vec();
    line.resize(LINE_LIMIT, b'x');
    line.extend_from_slice(terminator.as_bytes());
    line
}

#[test_case(Source::Stdout, "\n"; "stdout lf")]
#[test_case(Source::Stdout, "\r\n"; "stdout crlf")]
#[test_case(Source::Stdout, ""; "stdout unterminated")]
#[test_case(Source::Stderr, "\n"; "stderr lf")]
#[test_case(Source::Stderr, ""; "stderr unterminated")]
#[test_case(Source::Watched, "\n"; "watched lf")]
#[test_case(Source::Watched, "\r\n"; "watched crlf")]
#[test_case(Source::Watched, ""; "watched unterminated")]
#[tokio::test(start_paused = true)]
async fn a_line_of_exactly_the_limit_is_accepted_in_every_source(source: Source, end: &str) {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        let line = exact_limit_line(end);
        let unterminated_pipe = end.is_empty() && !matches!(source, Source::Watched);
        rig.fake.on_launch(move |n, command| {
            let (bytes, file) = (line.clone(), results(command));
            let _ = n;
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                match source {
                    Source::Stdout => {
                        let _ = ctx.stdout.write_all(&bytes).await;
                    }
                    Source::Stderr => {
                        let _ = ctx.stderr.write_all(&bytes).await;
                    }
                    Source::Watched => std::fs::write(file, &bytes).unwrap(),
                }
                if unterminated_pipe {
                    ctx.exit(ExitInfo::Code(0));
                } else {
                    ctx.until_killed().await;
                }
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(3))).await.unwrap();
        let run = finish(&handle).await;
        assert_eq!(
            run.finished.result.failure.map(|f| f.signal),
            Some(Signal::ComputationError)
        );
        assert_eq!(run.finished.result.inconclusive, None);
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

#[test_case(false, false; "cap then compute")]
#[test_case(true, false; "compute then cap")]
#[test_case(false, true; "cap then affinity")]
#[test_case(true, true; "affinity then cap")]
#[tokio::test(start_paused = true)]
async fn computation_and_affinity_errors_win_over_the_cap_in_either_order(
    line_first: bool,
    affinity: bool,
) {
    local(async {
        let rig = rig_with(TestBackend::new("/bin/true"), Timing::default());
        rig.fake.on_launch(move |_, _| {
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                let text = if affinity {
                    "AFFINITY:3\n"
                } else {
                    "COMPUTE ERROR\n"
                };
                if line_first {
                    ctx.eprint(text).await;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    let _ = ctx.stdout.write_all(&vec![b'x'; 1 << 20]).await;
                } else {
                    let _ = ctx.stdout.write_all(&vec![b'x'; 1 << 20]).await;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    ctx.eprint(text).await;
                }
                ctx.until_killed_ignoring_term().await;
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(60))).await.unwrap();
        let run = finish(&handle).await;
        let result = &run.finished.result;
        assert_eq!(result.inconclusive, None);
        if affinity {
            assert_eq!(result.escaped, [CpuId(3)]);
            assert_eq!(result.failure, None);
        } else {
            assert_eq!(
                result.failure.map(|f| f.signal),
                Some(Signal::ComputationError)
            );
            assert!(result.escaped.is_empty());
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_cap_never_hides_cleanup_or_ordinary_io_errors() {
    local(async {
        let rig = rig_with(TestBackend::new("/bin/true"), Timing::default());
        rig.fake
            .on_launch(emits_lingering(Source::Stderr, vec![b'x'; 1 << 20], true));
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { .. } => Some(Reply::Fail(togi_command::Error::Spawn {
                program: "systemctl".into(),
                source: std::io::Error::other("no bus"),
            })),
            _ => None,
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(60))).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.fake.process(0).fail_stdout();
        let run = finish(&handle).await;
        assert_eq!(
            run.finished.result.inconclusive.as_deref(),
            Some("core 00 read stderr.log: backend output line exceeds 64 KiB")
        );
        assert!(
            matches!(&run.finished.errors[..], [Error::Output(_)]),
            "{:?}",
            run.finished.errors
        );
        let containment = run.finished.teardown.unwrap_err();
        assert!(
            containment
                .failures()
                .iter()
                .any(|failure| matches!(failure, CleanupFailure::KillScope { .. }))
        );
        assert!(
            containment
                .failures()
                .iter()
                .any(|failure| matches!(failure, CleanupFailure::ReadPipe { .. }))
        );
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_cap_diagnostic_prevents_a_false_unexpected_exit() {
    local(async {
        let rig = rig_with(TestBackend::new("/bin/true"), Timing::default());
        rig.fake.on_launch(|_, _| {
            Launch::Run(script(|mut ctx: ProcCtx| async move {
                let _ = ctx.stdout.write_all(&vec![b'x'; 1 << 20]).await;
                ctx.exit(ExitInfo::Code(0));
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(60))).await.unwrap();
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.failure, None);
        assert!(run.finished.result.inconclusive.is_some());
    })
    .await;
}

fn flood(lines: usize) -> Vec<u8> {
    b"ok line\n".repeat(lines)
}

#[tokio::test(start_paused = true)]
async fn a_watched_flood_with_a_final_unterminated_error_is_drained_once_without_containment() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        let mut bytes = flood(4 * 1024 * 1024 / 8);
        bytes.extend_from_slice(b"COMPUTE ERROR");
        rig.fake.on_launch(move |_, command| {
            let (bytes, file) = (bytes.clone(), results(command));
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                std::fs::write(file, &bytes).unwrap();
                ctx.until_killed_ignoring_term().await;
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(3))).await.unwrap();
        let run = finish(&handle).await;
        assert_eq!(
            run.finished.result.failure.map(|f| f.signal),
            Some(Signal::ComputationError)
        );
        assert_eq!(run.signals().len(), 1);
        assert!(run.finished.teardown.is_ok());
        assert_eq!(run.finished.result.ran, secs(3));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_error_beyond_the_first_read_batches_is_found_by_the_teardown_drain() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        let mut bytes = flood(4 * 1024 * 1024 / 8);
        bytes.extend_from_slice(b"COMPUTE ERROR\n");
        rig.fake.on_launch(move |_, command| {
            let (bytes, file) = (bytes.clone(), results(command));
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                std::fs::write(file, &bytes).unwrap();
                ctx.until_killed().await;
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(2))).await.unwrap();
        let run = finish(&handle).await;
        assert_eq!(
            run.finished.result.failure.map(|f| f.signal),
            Some(Signal::ComputationError)
        );
        assert_eq!(run.finished.result.ran, secs(2));
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_flooding_file_cannot_starve_the_quiet_instance_beside_it() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        rig.fake.on_launch(|n, command| {
            let file = results(command);
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                if n == 0 {
                    std::fs::write(file, flood(4 * 1024 * 1024 / 8)).unwrap();
                } else {
                    std::fs::write(file, "COMPUTE ERROR\n").unwrap();
                }
                ctx.until_killed().await;
            }))
        });
        let handle = rig
            .start(spec(Regime::R7, &[0, 1], secs(600)))
            .await
            .unwrap();
        let run = finish(&handle).await;
        let failure = run.finished.result.failure.unwrap();
        assert_eq!(failure.core, super::harness::core(1));
        assert!(run.finished.result.ran <= secs(2));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_flooded_instance_does_not_skip_the_required_samples_of_its_neighbour() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        rig.fake.on_launch(|n, command| {
            let file = results(command);
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                if n == 0 {
                    std::fs::write(file, flood(4 * 1024 * 1024 / 8)).unwrap();
                }
                ctx.until_killed().await;
            }))
        });
        let handle = rig
            .start(spec(Regime::R7, &[0, 1], secs(600)))
            .await
            .unwrap();
        *rig.fake.process(1).threads.borrow_mut() = Ok(Threads {
            threads: vec![Thread { tid: 5, cpu: 7 }],
            errors: Vec::new(),
        });
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.escaped, [CpuId(7)]);
        assert!(run.finished.result.ran <= secs(2));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_flood_leaves_the_load_step_schedule_untouched() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        rig.fake.on_launch(|_, command| {
            let file = results(command);
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                std::fs::write(file, flood(4 * 1024 * 1024 / 8)).unwrap();
                ctx.until_killed().await;
            }))
        });
        let handle = rig.start(spec(Regime::R4, &[0], secs(1))).await.unwrap();
        let t0 = tokio::time::Instant::now();
        let run = finish(&handle).await;
        let steps: Vec<_> = rig
            .fake
            .process(0)
            .signal_times
            .borrow()
            .iter()
            .filter(|(at, signal)| {
                at.duration_since(t0) < secs(1)
                    && matches!(signal, GroupSignal::Stop | GroupSignal::Cont)
            })
            .map(|(at, _)| at.duration_since(t0).as_millis())
            .collect();
        let load = crate::plan::Load::Steps(
            togi_machine::LoadSchedule::for_spec(&spec(Regime::R4, &[0], secs(1))).unwrap(),
        );
        let expected: Vec<u128> = crate::schedule::timeline(&load, secs(1))
            .map(|step| step.at.as_millis())
            .collect();
        assert!(expected.len() >= 10);
        assert_eq!(steps, expected);
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

fn seeded_lines() -> Vec<u8> {
    flood(30_000)
}

#[test_case("remove"; "removed")]
#[test_case("truncate"; "truncated")]
#[test_case("replace"; "replaced by another inode")]
#[test_case("symlink"; "replaced by a symlink")]
#[tokio::test(start_paused = true)]
async fn unread_accepted_watched_bytes_that_vanish_are_lost_output_and_containment(how: &str) {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        let how = how.to_owned();
        rig.fake.on_launch(move |_, command| {
            let (file, how) = (results(command), how.clone());
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                std::fs::write(&file, seeded_lines()).unwrap();
                tokio::time::sleep(Duration::from_millis(1500)).await;
                match how.as_str() {
                    "remove" => std::fs::remove_file(&file).unwrap(),
                    "truncate" => std::fs::write(&file, "short\n").unwrap(),
                    "replace" => {
                        let other = file.with_file_name("other");
                        std::fs::write(&other, seeded_lines()).unwrap();
                        std::fs::rename(other, &file).unwrap();
                    }
                    _ => {
                        std::fs::remove_file(&file).unwrap();
                        symlink("/dev/null", &file).unwrap();
                    }
                }
                ctx.until_killed().await;
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(5))).await.unwrap();
        let run = finish(&handle).await;
        let containment = run.finished.teardown.unwrap_err();
        assert!(
            containment
                .failures()
                .iter()
                .any(|failure| matches!(failure, CleanupFailure::WatchedLost { .. }))
        );
        assert!(run.finished.result.inconclusive.is_some());
        assert_eq!(run.finished.result.failure, None);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn rejecting_a_symlinked_watched_file_without_backlog_is_an_ordinary_diagnostic() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        rig.fake.on_launch(|_, command| {
            let file = results(command);
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                symlink("/etc/hostname", &file).unwrap();
                ctx.until_killed().await;
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(4))).await.unwrap();
        let run = finish(&handle).await;
        assert!(run.finished.teardown.is_ok());
        assert_eq!(run.finished.result.ran, secs(4));
        assert_eq!(
            run.finished.result.inconclusive.as_deref(),
            Some("core 00 watched output: unsupported watched output file")
        );
        assert!(matches!(&run.finished.errors[..], [Error::Output(_)]));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_watched_file_that_is_never_created_is_harmless() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        let handle = rig.start(spec(Regime::R1, &[0], secs(3))).await.unwrap();
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.ran, secs(3));
        assert_eq!(run.finished.result.inconclusive, None);
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_watched_partial_is_classified_only_after_the_scope_kill_confirms_the_writers() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        rig.fake.on_launch(|_, command| {
            let file = results(command);
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                std::fs::write(&file, "COMPUTE").unwrap();
                while let Some(delivery) = ctx.signals.recv().await {
                    match delivery {
                        Delivery::Group(GroupSignal::Term) => {
                            use std::io::Write as _;
                            let mut handle = std::fs::OpenOptions::new()
                                .append(true)
                                .open(&file)
                                .unwrap();
                            handle.write_all(b" ERROR").unwrap();
                        }
                        Delivery::Scope(_) => {
                            ctx.exit(ExitInfo::Signal(9));
                            return;
                        }
                        Delivery::Group(_) => {}
                    }
                }
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(2))).await.unwrap();
        let run = finish(&handle).await;
        assert_eq!(
            run.finished.result.failure.map(|f| f.signal),
            Some(Signal::ComputationError)
        );
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn an_exit_queued_ahead_of_a_watched_error_beyond_the_first_batch_does_not_win() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        let mut bytes = flood(25_000);
        bytes.extend_from_slice(b"COMPUTE ERROR\n");
        rig.fake.on_launch(move |_, command| {
            let (bytes, file) = (bytes.clone(), results(command));
            Launch::Run(script(move |ctx: ProcCtx| async move {
                std::fs::write(file, &bytes).unwrap();
                ctx.exit(ExitInfo::Code(1));
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(60))).await.unwrap();
        let run = finish(&handle).await;
        assert_eq!(
            run.finished.result.failure.map(|f| f.signal),
            Some(Signal::ComputationError)
        );
    })
    .await;
}

#[test_case(true, "hello\n", true; "unscoped helper with a confirmed group kill")]
#[test_case(false, "COMPUTE ERROR", false; "unscoped helper whose group is already gone")]
#[tokio::test(start_paused = true)]
async fn an_unscoped_watched_partial_needs_a_group_kill_that_actually_succeeded(
    kill_succeeds: bool,
    _text: &str,
    classified: bool,
) {
    local(async {
        let rig = rig_edit(watching(), Timing::default(), |env| env.unscoped = true);
        rig.fake.on_launch(|_, command| {
            let file = results(command);
            Launch::Run(script(move |ctx: ProcCtx| async move {
                std::fs::write(&file, "COMPUTE ERROR").unwrap();
                tokio::time::sleep(Duration::from_millis(2500)).await;
                ctx.exit(ExitInfo::Code(0));
            }))
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(2))).await.unwrap();
        if !kill_succeeds {
            *rig.fake.process(0).signal_error.borrow_mut() = Some(SignalErrorKind::Gone);
        }
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.failure.is_some(), kill_succeeds);
        assert_eq!(run.finished.teardown.is_ok(), kill_succeeds);
        let _ = classified;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn writers_confirmed_for_one_instance_keep_their_evidence_when_another_scope_fails() {
    local(async {
        let rig = rig_with(watching(), Timing::default());
        rig.fake.on_launch(|n, command| {
            let file = results(command);
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                std::fs::write(&file, "COMPUTE ERROR").unwrap();
                let _ = n;
                ctx.until_killed().await;
            }))
        });
        rig.fake.on_systemd(|call| match call {
            Systemd::Kill { scope, .. } if scope.as_str().ends_with("c00") => {
                Some(Reply::Fail(togi_command::Error::Spawn {
                    program: "systemctl".into(),
                    source: std::io::Error::other("no bus"),
                }))
            }
            _ => None,
        });
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R7, &[0, 1], secs(60)))
            .await
            .unwrap();
        tokio::time::sleep(secs(2)).await;
        cancel.cancel();
        let run = finish(&handle).await;
        let failure = run.finished.result.failure.unwrap();
        assert_eq!(
            failure.core,
            super::harness::core(1),
            "c01's writers were confirmed"
        );
        assert!(run.finished.teardown.is_err());
    })
    .await;
}

struct Blocked(std::sync::mpsc::Receiver<()>);

impl std::io::Write for Blocked {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let _ = self.0.recv();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn a_blocked_log_write_never_stalls_supervision_and_is_reported() {
    local(async {
        let gates = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = std::sync::Arc::clone(&gates);
        let timing = Timing {
            writer_join: Duration::from_millis(300),
            ..Timing::default()
        };
        let rig = rig_edit(TestBackend::new("/bin/true"), timing, move |env| {
            let gates = std::sync::Arc::clone(&kept);
            env.log_sink = Some(std::rc::Rc::new(move || {
                let gates = std::sync::Arc::clone(&gates);
                Box::new(move |_file| {
                    let (gate, blocked) = std::sync::mpsc::channel::<()>();
                    gates.lock().unwrap().push(gate);
                    Box::new(Blocked(blocked)) as crate::logs::Sink
                })
            }));
        });
        rig.fake
            .on_launch(emits(Source::Stdout, b"ok line\n".repeat(512 * 1024)));
        let handle = rig.start(spec(Regime::R1, &[0], secs(3))).await.unwrap();
        let wall = std::time::Instant::now();
        let run = finish(&handle).await;
        assert!(wall.elapsed() < secs(10));
        assert_eq!(run.finished.result.ran, secs(3));
        assert!(run.finished.teardown.is_ok());
        let Some(Error::Output(output)) = run.finished.errors.last() else {
            panic!("{:?}", run.finished.errors);
        };
        assert!(
            output
                .diagnostics()
                .iter()
                .any(|d| d.contains("output bytes were dropped"))
        );
        assert!(
            output
                .diagnostics()
                .iter()
                .any(|d| d.contains("did not finish"))
        );
        drop(gates);
    })
    .await;
}
