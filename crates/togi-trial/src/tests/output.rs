//! Output framing, the 64 KiB cap, watched files and their evidence under teardown.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use test_case::test_case;
use togi_backend::{Backend, Line, Plan, Ready};
use togi_machine::{CancellationToken, CpuId, Regime, Signal, Workload};
use tokio::io::AsyncWriteExt as _;

use super::harness::{Landed, finish, local, rig_edit, rig_with, spec};
use crate::error::{CleanupFailure, Error};
use crate::os::fake::{Delivery, Launch, ProcCtx, Reply, SignalErrorKind, script};
use crate::os::{ExitInfo, GroupSignal, Systemd};
use crate::output::LINE_LIMIT;
use crate::procfs::{Thread, Threads};
use crate::runner::Backends;
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
                Source::Watched => ctx.write_file(file, &bytes).unwrap(),
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
    rig.read(rig.dir.path().join("trials/t1/work").join(name))
        .unwrap()
}

#[test_case(Source::Stdout, "stdout.log"; "stdout")]
#[test_case(Source::Stderr, "stderr.log"; "stderr")]
#[test_case(Source::Watched, "watched output"; "watched file")]
fn an_oversized_line_is_inconclusive_keeps_its_prefix_and_is_not_containment(
    source: Source,
    name: &str,
) {
    crate::testing::runtime(true).block_on(async move {
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
            assert!(
                matches!(&run.finished.errors[..], [Error::Output(_)]),
                "the cap is a runner error without stronger evidence: {:?}",
                run.finished.errors
            );
            match source {
                Source::Stdout => assert_eq!(log(&rig, "stdout.log"), vec![b'x'; LINE_LIMIT]),
                Source::Stderr => assert_eq!(log(&rig, "stderr.log"), vec![b'x'; LINE_LIMIT]),
                Source::Watched => {}
            }
        })
        .await;
    });
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
fn a_line_of_exactly_the_limit_is_accepted_in_every_source(source: Source, end: &str) {
    crate::testing::runtime(true).block_on(async move {
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
                        Source::Watched => ctx.write_file(file, &bytes).unwrap(),
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
    });
}

#[test_case(false, false; "cap then compute")]
#[test_case(true, false; "compute then cap")]
#[test_case(false, true; "cap then affinity")]
#[test_case(true, true; "affinity then cap")]
fn computation_and_affinity_errors_win_over_the_cap_in_either_order(
    line_first: bool,
    affinity: bool,
) {
    crate::testing::runtime(true).block_on(async move {
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
                assert_eq!(result.escaped, [CpuId::from(3)]);
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
    });
}

#[test]
fn the_cap_never_hides_cleanup_or_ordinary_io_errors() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(TestBackend::new("/bin/true"), Timing::default());
            rig.fake
                .on_launch(emits_lingering(Source::Stderr, vec![b'x'; 1 << 20], true));
            rig.fake.on_systemd(|call| {
                if let Systemd::Kill { .. } = call {
                    Some(Reply::Fail(togi_command::Error::Spawn {
                        program: "systemctl".into(),
                        source: std::io::Error::other("no bus"),
                    }))
                } else {
                    None
                }
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
    });
}

#[test]
fn the_cap_diagnostic_prevents_a_false_unexpected_exit() {
    crate::testing::runtime(true).block_on(async move {
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
    });
}

fn flood(lines: usize) -> Vec<u8> {
    b"ok line\n".repeat(lines)
}

#[test]
fn a_watched_flood_with_a_final_unterminated_error_is_drained_once_without_containment() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            let mut bytes = flood(4 * 1024 * 1024 / 8);
            bytes.extend_from_slice(b"COMPUTE ERROR");
            rig.fake.on_launch(move |_, command| {
                let (bytes, file) = (bytes.clone(), results(command));
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(file, &bytes).unwrap();
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
    });
}

#[test]
fn an_error_beyond_the_first_read_batches_is_found_by_the_teardown_drain() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            let mut bytes = flood(4 * 1024 * 1024 / 8);
            bytes.extend_from_slice(b"COMPUTE ERROR\n");
            rig.fake.on_launch(move |_, command| {
                let (bytes, file) = (bytes.clone(), results(command));
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(file, &bytes).unwrap();
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
    });
}

#[test]
fn a_flooding_file_cannot_starve_the_quiet_instance_beside_it() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            rig.fake.on_launch(|n, command| {
                let file = results(command);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    if n == 0 {
                        ctx.write_file(file, flood(4 * 1024 * 1024 / 8)).unwrap();
                    } else {
                        ctx.write_file(file, "COMPUTE ERROR\n").unwrap();
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
    });
}

#[test]
fn a_flooded_instance_does_not_skip_the_required_samples_of_its_neighbour() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            rig.fake.on_launch(|n, command| {
                let file = results(command);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    if n == 0 {
                        ctx.write_file(file, flood(4 * 1024 * 1024 / 8)).unwrap();
                    }
                    ctx.until_killed().await;
                }))
            });
            let handle = rig
                .start(spec(Regime::R7, &[0, 1], secs(600)))
                .await
                .unwrap();
            *rig.fake.process(1).threads.borrow_mut() = Ok(Threads {
                threads: vec![Thread {
                    tid: 5,
                    cpu: CpuId::from(7),
                }],
                errors: Vec::new(),
            });
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.escaped, [CpuId::from(7)]);
            assert!(run.finished.result.ran <= secs(2));
        })
        .await;
    });
}

#[test]
fn every_ready_instance_is_sampled_each_tick_while_both_watched_floods_are_outstanding() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let landed = Arc::new(Landed::default());
            let landed_before = Arc::clone(&landed);
            let rig = rig_edit(watching(), Timing::default(), |env| {
                env.storage = Some(landed.storage());
            });
            rig.fake.on_launch(|_, command| {
                let file = results(command);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(file, flood(4 * 1024 * 1024 / 8)).unwrap();
                    ctx.until_killed().await;
                }))
            });
            let cancel = CancellationToken::new();
            let threads_seen = Rc::new(RefCell::new(BTreeMap::<u32, usize>::new()));
            let (seen, cancelling) = (Rc::clone(&threads_seen), cancel.clone());
            rig.fake.on_threads(move |pid| {
                let mut seen = seen.borrow_mut();
                let count = seen.entry(pid).or_default();
                *count += 1;
                landed_before.wait_for(*count - 1);
                if seen.len() == 2 && seen.values().all(|&count| count == 3) {
                    cancelling.cancel();
                }
            });
            let begun = tokio::time::Instant::now();
            let handle = rig
                .start_with(&cancel, spec(Regime::R7, &[0, 1], secs(600)))
                .await
                .unwrap();
            let run = finish(&handle).await;
            let pids = [rig.fake.process(0).pid, rig.fake.process(1).pid];
            assert_eq!(
                *threads_seen.borrow(),
                BTreeMap::from([(pids[0], 3), (pids[1], 3)])
            );
            let samples: Vec<_> = landed.samples();
            for core in [super::harness::core(0), super::harness::core(1)] {
                let usage_seen = samples
                    .iter()
                    .filter(|sample| sample.worker_cpu_ms.get(core).is_some())
                    .count();
                assert_eq!(usage_seen, 3, "usage observations of {core:?}");
            }
            assert!(
                matches!(run.finished.errors[..], [Error::Cancelled]),
                "{:?}",
                run.finished.errors
            );
            assert!(run.finished.result.failure.is_none());
            assert_eq!(run.finished.result.ran, secs(3));
            assert!(run.finished.teardown.is_ok());
            assert!(begun.elapsed() <= secs(4), "{:?}", begun.elapsed());
        })
        .await;
    });
}

/// The watching test backend whose classifier takes a controlled real time per `SLOW` line and
/// stamps the start of each one.
#[derive(Debug)]
struct Slow {
    inner: TestBackend,
    nap: Duration,
    stamps: Arc<Mutex<Vec<std::time::Instant>>>,
}

impl Backend for Slow {
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
        if line.starts_with("SLOW") {
            self.stamps.lock().unwrap().push(std::time::Instant::now());
            std::thread::sleep(self.nap);
        }
        self.inner.classify(line)
    }
}

#[test]
fn a_slow_classified_watched_flood_is_polled_during_the_trial_and_leaves_the_load_steps_on_time() {
    crate::testing::runtime(false).block_on(async move {
        local(async {
    let stamps = Arc::new(Mutex::new(Vec::new()));
    let slow = Slow {
        inner: watching(),
        nap: Duration::from_millis(2),
        stamps: Arc::clone(&stamps),
    };
    let timing = Timing {
        sample: Duration::from_millis(25),
        ..Timing::default()
    };
    let rig = rig_edit(watching(), timing, move |env| {
        env.backends = Backends::new().with(Arc::new(slow));
    });
    rig.fake.on_launch(|_, command| {
        let file = results(command);
        Launch::Run(script(move |mut ctx: ProcCtx| async move {
            ctx.write_file(file, "SLOW ok line\n".repeat(400)).unwrap();
            ctx.until_killed().await;
        }))
    });
    let handle = rig.start(spec(Regime::R4, &[0], secs(1))).await.unwrap();
    let started = std::time::Instant::now();
    let run = finish(&handle).await;
    let classified = *stamps
        .lock()
        .unwrap()
        .first()
        .expect("the flood was classified");
    let terminated = rig
        .fake
        .process(0)
        .signal_times
        .borrow()
        .iter()
        .find(|(_, signal)| matches!(signal, GroupSignal::Term))
        .map(|(at, _)| at.into_std())
        .expect("the teardown terminated the worker");
    assert!(
        classified < terminated,
        "watched output is polled by the sampling ticks of the trial, not first by the \
         teardown"
    );
    let toggles: Vec<_> = rig
        .fake
        .process(0)
        .signal_times
        .borrow()
        .iter()
        .filter(|(_, signal)| matches!(signal, GroupSignal::Stop | GroupSignal::Cont))
        .map(|(at, _)| at.into_std())
        .collect();
    let load = crate::plan::Load::Steps(
        togi_machine::LoadSchedule::for_spec(&spec(Regime::R4, &[0], secs(1))).unwrap(),
    );
    let planned: Vec<_> = crate::schedule::timeline(&load, secs(1))
        .map(|step| step.at)
        .collect();
    assert!(planned.len() >= 10 && toggles.len() >= planned.len());
    let stamps = stamps.lock().unwrap();
    for (index, (toggle, at)) in toggles.iter().zip(&planned).enumerate() {
        let due = started + *at + timing.sample;
        let overtaken = stamps
            .iter()
            .filter(|&&stamp| due < stamp && stamp < *toggle)
            .count();
        assert_eq!(
            overtaken, 0,
            "step {index} planned at {at:?} waited for {overtaken} classifications that began \
             more than one sample after it was due"
        );
    }
    assert!(run.finished.teardown.is_ok());
})
.await;
    });
}

#[test]
fn a_hard_linked_watched_file_is_refused_as_an_ordinary_diagnostic() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
    let rig = rig_with(watching(), Timing::default());
    rig.fake.on_launch(|_, command| {
        let file = results(command);
        Launch::Run(script(move |mut ctx: ProcCtx| async move {
            let elsewhere = file.with_file_name("elsewhere");
            ctx.write_file(&elsewhere, "COMPUTE ERROR\n").unwrap();
            ctx.memory.hard_link(&elsewhere,&file).unwrap();
            ctx.until_killed().await;
        }))
    });
    let handle = rig.start(spec(Regime::R1, &[0], secs(3))).await.unwrap();
    let run = finish(&handle).await;
    assert!(run.finished.teardown.is_ok());
    assert_eq!(run.finished.result.failure, None);
    assert_eq!(
        run.finished.result.inconclusive.as_deref(),
        Some(
            "core 00 watched output: refused watched output file: the file has other hard links"
        )
    );
    assert!(matches!(&run.finished.errors[..], [Error::Output(_)]));
})
.await;
    });
}

fn seeded_lines() -> Vec<u8> {
    flood(30_000)
}

#[test_case("remove"; "removed")]
#[test_case("truncate"; "truncated")]
#[test_case("replace"; "replaced by another inode")]
#[test_case("symlink"; "replaced by a symlink")]
#[test_case("hardlink"; "accepted inode gains another hard link")]
#[test_case("fifo"; "replaced by a nonregular file")]
fn unread_accepted_watched_bytes_that_vanish_are_lost_output_and_containment(how: &str) {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            let how = how.to_owned();
            rig.fake.on_launch(move |_, command| {
                let (file, how) = (results(command), how.clone());
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(&file, seeded_lines()).unwrap();
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    match how.as_str() {
                        "remove" => ctx.memory.remove_file(&file).unwrap(),
                        "truncate" => ctx.write_file(&file, "short\n").unwrap(),
                        "replace" => {
                            let other = file.with_file_name("other");
                            ctx.write_file(&other, seeded_lines()).unwrap();
                            ctx.memory.rename(other, &file).unwrap();
                        }
                        "hardlink" => {
                            ctx.memory
                                .hard_link(&file, file.with_file_name("elsewhere"))
                                .unwrap();
                        }
                        "fifo" => {
                            ctx.memory.remove_file(&file).unwrap();
                            ctx.memory.nonregular(&file).unwrap();
                        }
                        _ => {
                            ctx.memory.remove_file(&file).unwrap();
                            ctx.memory.symlink("/dev/null", &file).unwrap();
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
    });
}

#[test]
fn rejecting_a_symlinked_watched_file_without_backlog_is_an_ordinary_diagnostic() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            rig.fake.on_launch(|_, command| {
                let file = results(command);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.memory.symlink("/etc/hostname", &file).unwrap();
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
    });
}

#[test]
fn a_watched_file_that_is_never_created_is_harmless() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            let handle = rig.start(spec(Regime::R1, &[0], secs(3))).await.unwrap();
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.ran, secs(3));
            assert_eq!(run.finished.result.inconclusive, None);
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn a_watched_partial_is_classified_only_after_the_scope_kill_confirms_the_writers() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            rig.fake.on_launch(|_, command| {
                let file = results(command);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(&file, "COMPUTE").unwrap();
                    while let Some(delivery) = ctx.signals.recv().await {
                        match delivery {
                            Delivery::Group(GroupSignal::Term) => {
                                ctx.memory.append_file(&file, b" ERROR", None).unwrap();
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
    });
}

#[test]
fn an_exit_queued_ahead_of_a_watched_error_beyond_the_first_batch_does_not_win() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            let mut bytes = flood(25_000);
            bytes.extend_from_slice(b"COMPUTE ERROR\n");
            rig.fake.on_launch(move |_, command| {
                let (bytes, file) = (bytes.clone(), results(command));
                Launch::Run(script(move |ctx: ProcCtx| async move {
                    ctx.write_file(file, &bytes).unwrap();
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
    });
}

#[test_case(true, "hello\n", true; "unscoped helper with a confirmed group kill")]
#[test_case(false, "COMPUTE ERROR", false; "unscoped helper whose group is already gone")]
fn an_unscoped_watched_partial_needs_a_group_kill_that_actually_succeeded(
    kill_succeeds: bool,
    _text: &str,
    classified: bool,
) {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_edit(watching(), Timing::default(), |env| env.unscoped = true);
            rig.fake.on_launch(|_, command| {
                let file = results(command);
                Launch::Run(script(move |ctx: ProcCtx| async move {
                    ctx.write_file(&file, "COMPUTE ERROR").unwrap();
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
    });
}

#[test]
fn writers_confirmed_for_one_instance_keep_their_evidence_when_another_scope_fails() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            rig.fake.on_launch(|n, command| {
                let file = results(command);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(&file, "COMPUTE ERROR").unwrap();
                    let _ = n;
                    ctx.until_killed().await;
                }))
            });
            rig.fake.on_systemd(|call| {
                if let Systemd::Kill { scope, .. } = call
                    && scope.as_str().ends_with("c00")
                {
                    Some(Reply::Fail(togi_command::Error::Spawn {
                        program: "systemctl".into(),
                        source: std::io::Error::other("no bus"),
                    }))
                } else {
                    None
                }
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
    });
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

#[test]
fn a_blocked_log_write_never_stalls_supervision_and_is_reported() {
    crate::testing::runtime(true).block_on(async move {
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
            let begun = tokio::time::Instant::now();
            let run = finish(&handle).await;
            assert!(begun.elapsed() < rig.env.timing.wait_bound(secs(3)));
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
    });
}

#[test_case("PIN FAILED", None; "setup")]
#[test_case("AFFINITY:3", Some(CpuId::from(3)); "affinity")]
fn an_exit_queued_ahead_of_watched_setup_or_affinity_text_beyond_the_first_batch_does_not_win(
    line: &'static str,
    escaped: Option<CpuId>,
) {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            let mut bytes = flood(25_000);
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
            rig.fake.on_launch(move |_, command| {
                let (bytes, file) = (bytes.clone(), results(command));
                Launch::Run(script(move |ctx: ProcCtx| async move {
                    ctx.write_file(file, &bytes).unwrap();
                    ctx.exit(ExitInfo::Code(1));
                }))
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(60))).await.unwrap();
            let run = finish(&handle).await;
            let result = &run.finished.result;
            assert_eq!(result.failure, None);
            assert!(run.signals().is_empty(), "{:?}", run.signals());
            assert_eq!(result.escaped, Vec::from_iter(escaped));
            assert_eq!(result.inconclusive.is_some(), escaped.is_none());
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn an_unscoped_watched_partial_after_a_failed_group_kill_is_withheld_as_containment() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_edit(watching(), Timing::default(), |env| env.unscoped = true);
            rig.fake.on_launch(|_, command| {
                let file = results(command);
                Launch::Run(script(move |ctx: ProcCtx| async move {
                    ctx.write_file(&file, "COMPUTE ERROR").unwrap();
                    tokio::time::sleep(Duration::from_millis(2500)).await;
                    ctx.exit(ExitInfo::Code(0));
                }))
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(2))).await.unwrap();
            *rig.fake.process(0).signal_error.borrow_mut() = Some(SignalErrorKind::Failed);
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.failure, None);
            assert!(run.signals().is_empty(), "{:?}", run.signals());
            let containment = run.finished.teardown.unwrap_err();
            assert!(
                containment
                    .failures()
                    .iter()
                    .any(|failure| matches!(failure, CleanupFailure::SignalGroup { .. })),
                "{:?}",
                containment.failures()
            );
        })
        .await;
    });
}

#[test]
fn writers_confirmed_for_one_instance_keep_their_evidence_when_another_group_kill_fails() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            rig.fake.on_launch(|_, command| {
                let file = results(command);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(&file, "COMPUTE ERROR").unwrap();
                    ctx.until_killed().await;
                }))
            });
            let cancel = CancellationToken::new();
            let handle = rig
                .start_with(&cancel, spec(Regime::R7, &[0, 1], secs(60)))
                .await
                .unwrap();
            *rig.fake.process(0).signal_error.borrow_mut() = Some(SignalErrorKind::Failed);
            tokio::time::sleep(secs(2)).await;
            cancel.cancel();
            let run = finish(&handle).await;
            let failure = run.finished.result.failure.unwrap();
            assert_eq!(failure.core, super::harness::core(1));
            assert_eq!(
                run.signals(),
                [(super::harness::core(1), Signal::ComputationError)]
            );
            let containment = run.finished.teardown.unwrap_err();
            assert!(
                containment
                    .failures()
                    .iter()
                    .any(|failure| matches!(failure, CleanupFailure::SignalGroup { .. }))
            );
        })
        .await;
    });
}

#[test]
fn writers_of_an_instance_whose_scope_is_already_gone_keep_their_evidence_when_another_scope_fails()
{
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            rig.fake.on_launch(|_, command| {
                let file = results(command);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.write_file(&file, "COMPUTE ERROR").unwrap();
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
                Systemd::Kill { .. } => Some(Reply::Output {
                    success: false,
                    text: "Failed to kill unit: Unit could not be found.".into(),
                }),
                Systemd::Stop { .. } | Systemd::ListScopes | Systemd::Probe { .. } => None,
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
            assert_eq!(failure.core, super::harness::core(1));
            assert_eq!(
                run.signals(),
                [(super::harness::core(1), Signal::ComputationError)]
            );
            let containment = run.finished.teardown.unwrap_err();
            assert!(
                containment
                    .failures()
                    .iter()
                    .any(|failure| matches!(failure, CleanupFailure::KillScope { .. }))
            );
        })
        .await;
    });
}

#[test]
fn rejecting_a_fifo_watched_file_without_backlog_is_an_ordinary_diagnostic() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_with(watching(), Timing::default());
            rig.fake.on_launch(|_, command| {
                let file = results(command);
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.memory.nonregular(&file).unwrap();
                    ctx.until_killed().await;
                }))
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(4))).await.unwrap();
            let run = finish(&handle).await;
            assert!(run.finished.teardown.is_ok());
            assert_eq!(run.finished.result.ran, secs(4));
            assert_eq!(run.finished.result.failure, None);
            assert_eq!(
                run.finished.result.inconclusive.as_deref(),
                Some("core 00 watched output: unsupported watched output file")
            );
            assert!(matches!(&run.finished.errors[..], [Error::Output(_)]));
        })
        .await;
    });
}
