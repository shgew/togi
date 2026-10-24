//! Conditions samples through the supervisor: sensors, the writer and its failure modes.

use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use togi_machine::{CancellationToken, Regime};

use super::harness::{Landed, core, finish, local, rig, rig_edit, spec};
use super::trial::Recorder;
use crate::error::Error;
use crate::error::StorageError;
use crate::os::GroupSignal;
use crate::samples::{Open, Storage};
use crate::supervisor::{Event, Handle};
use crate::testing::TestBackend;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn write(rig: &super::harness::Rig, relative: &str, text: &str) {
    let path = rig.dir.path().join(relative);
    rig.write(path, text).unwrap();
}

#[test]
fn each_tick_persists_a_conditions_sample_with_sensors_workers_and_power() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let landed = Arc::new(Landed::default());
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
                env.storage = Some(landed.storage());
            });
            let tick = std::cell::Cell::new(0);
            let before_tick = Arc::clone(&landed);
            rig.fake.on_threads(move |_| {
                tick.set(tick.get() + 1);
                before_tick.wait_for(tick.get() - 1);
            });
            write(&rig, "hwmon/hwmon0/name", "k10temp\n");
            write(&rig, "hwmon/hwmon0/temp1_label", "Tctl\n");
            write(&rig, "hwmon/hwmon0/temp1_input", "62000\n");
            write(&rig, "hwmon/hwmon0/temp2_label", "Tccd1\n");
            write(&rig, "hwmon/hwmon0/temp2_input", "58500\n");
            write(&rig, "cpu/cpu2/cpufreq/scaling_cur_freq", "4500000\n");
            write(&rig, "cpu/cpu3/cpufreq/scaling_cur_freq", "1000000\n");
            write(&rig, "powercap/intel-rapl:0/energy_uj", "0\n");
            let handle = rig.start(spec(Regime::R1, &[2], secs(3))).await.unwrap();
            for (at, cpu_ms) in [(900_u64, 1000_u64), (1900, 2000)] {
                let process = rig.fake.process(0);
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(Duration::from_millis(at)).await;
                    *process.usage.borrow_mut() = Ok(crate::procfs::Usage {
                        cpu_time: Duration::from_millis(cpu_ms),
                        cpu: togi_machine::CpuId::from(2),
                    });
                });
            }
            let dir = rig.dir.path().to_owned();
            for (at, energy) in [(500, "10000000"), (1500, "20000000")] {
                let dir = dir.clone();
                let memory = rig.memory.clone();
                tokio::task::spawn_local(async move {
                    tokio::time::sleep(Duration::from_millis(at)).await;
                    memory
                        .write_file(dir.join("powercap/intel-rapl:0/energy_uj"), energy, None)
                        .unwrap();
                });
            }
            let run = finish(&handle).await;
            assert!(run.finished.errors.is_empty(), "{:?}", run.finished.errors);
            assert_eq!(run.finished.result.tctl_max_c, Some(62));
            let samples: Vec<_> = landed.samples();
            assert_eq!(samples.len(), 2);
            assert_eq!(samples[0].elapsed_ms, 1000);
            assert_eq!(samples[1].elapsed_ms, 2000);
            for (index, sample) in samples.iter().enumerate() {
                assert_eq!(sample.tctl_c, Some(62));
                assert_eq!(
                    sample.tccd_c.as_ref().and_then(|m| m.get("Tccd1")),
                    Some(&58)
                );
                assert_eq!(sample.core_mhz.get(core(2)), Some(4500));
                assert_eq!(sample.core_mhz.get(core(3)), Some(1000));
                assert_eq!(sample.core_mhz.len(), 2);
                assert_eq!(sample.package_power_w, Some(10.0));
                assert_eq!(
                    sample.worker_cpu_ms.get(core(2)),
                    Some(1000 * (i64::try_from(index).unwrap() + 1))
                );
                assert!(sample.pm_table.is_none());
            }
        })
        .await;
    });
}

#[test]
fn missing_sensors_are_omitted_not_zeroed() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(spec(Regime::R1, &[2], secs(2))).await.unwrap();
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.tctl_max_c, None);
            let samples: Vec<_> = rig.samples();
            assert_eq!(samples.len(), 1);
            let sample = &samples[0];
            assert!(sample.tctl_c.is_none() && sample.tccd_c.is_none());
            assert!(sample.core_mhz.is_empty() && sample.package_power_w.is_none());
        })
        .await;
    });
}

#[test]
fn overflowing_optional_sensor_is_a_runner_error_not_absent_telemetry() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            write(&rig, "hwmon/hwmon0/name", "k10temp\n");
            write(&rig, "hwmon/hwmon0/temp1_label", "Tctl\n");
            write(&rig, "hwmon/hwmon0/temp1_input", "9223372036854775808\n");
            let handle = rig.start(spec(Regime::R1, &[2], secs(2))).await.unwrap();
            let run = finish(&handle).await;
            assert!(
                run.finished
                    .errors
                    .iter()
                    .any(|error| matches!(error, Error::Sensor(_)))
            );
            assert!(rig.samples().is_empty());
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn overflowing_energy_baseline_rolls_back_the_launched_trial() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            write(
                &rig,
                "powercap/intel-rapl:0/energy_uj",
                "9223372036854775808\n",
            );
            let failure = rig
                .start(spec(Regime::R1, &[2], secs(2)))
                .await
                .err()
                .unwrap();
            assert!(failure.value.is_some());
            assert!(togi_machine::error_chain(&failure.error).contains("read numeric sensor"));
        })
        .await;
    });
}

#[test]
fn overflowing_cpu_milliseconds_is_sampling_loss_and_never_a_capped_sample() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_edit(
                TestBackend::new("/bin/true"),
                Timing {
                    stall_grace: secs(1),
                    stall_window: secs(1),
                    ..Timing::default()
                },
                |_| {},
            );
            let handle = rig.start(spec(Regime::R1, &[2], secs(5))).await.unwrap();
            *rig.fake.process(0).usage.borrow_mut() = Ok(crate::procfs::Usage {
                cpu_time: Duration::from_millis(i64::MAX.cast_unsigned() + 1),
                cpu: togi_machine::CpuId::from(2),
            });
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.failure, None);
            assert!(
                run.finished
                    .result
                    .inconclusive
                    .as_deref()
                    .unwrap()
                    .contains("cpu time")
            );
            assert!(
                rig.samples()
                    .iter()
                    .all(|sample| sample.worker_cpu_ms.is_empty())
            );
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

struct Blocking {
    gate: std::sync::mpsc::Receiver<()>,
    lines: Arc<Mutex<usize>>,
}

impl Storage for Blocking {
    fn append(&mut self, _line: &[u8]) -> std::io::Result<()> {
        let _ = self.gate.recv();
        *self.lines.lock().unwrap() += 1;
        Ok(())
    }

    fn close(self: Box<Self>) -> std::io::Result<()> {
        Ok(())
    }
}

fn blocked_open(
    gate: std::sync::mpsc::Receiver<()>,
    lines: Arc<Mutex<usize>>,
) -> Rc<dyn Fn() -> Result<Open, StorageError>> {
    let slot = Mutex::new(Some((gate, lines)));
    Rc::new(move || {
        let parts = slot.lock().unwrap().take();
        Ok(Box::new(move || {
            let (gate, lines) =
                parts.ok_or_else(|| StorageError::Create(std::io::Error::other("reused")))?;
            Ok(Box::new(Blocking { gate, lines }) as Box<dyn Storage>)
        }) as Open)
    })
}

#[test]
fn slow_storage_neither_delays_load_steps_nor_the_trial_and_the_join_is_bounded() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
    let (gate, gated) = std::sync::mpsc::channel::<()>();
    let lines = Arc::new(Mutex::new(0));
    let storage = blocked_open(gated, Arc::clone(&lines));
    let timing = Timing {
        writer_join: Duration::from_millis(100),
        ..Timing::default()
    };
    let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
        env.storage = Some(storage);
    });
    let handle = rig.start(spec(Regime::R4, &[0], secs(3))).await.unwrap();
    let t0 = tokio::time::Instant::now();
    let run = finish(&handle).await;
    let steps: Vec<_> = rig
        .fake
        .process(0)
        .signal_times
        .borrow()
        .iter()
        .filter(|(at, signal)| {
            at.duration_since(t0) < secs(3)
                && matches!(signal, GroupSignal::Stop | GroupSignal::Cont)
        })
        .map(|(at, _)| at.duration_since(t0).as_millis())
        .collect();
    let load = crate::plan::Load::Steps(
        togi_machine::LoadSchedule::for_spec(&spec(Regime::R4, &[0], secs(3))).unwrap(),
    );
    let expected: Vec<u128> = crate::schedule::timeline(&[load], secs(3))
        .map(|step| step.at.as_millis())
        .collect();
    assert_eq!(steps, expected);
    assert_eq!(run.finished.result.ran, secs(3));
    assert!(
        run.finished.teardown.is_ok(),
        "storage is never containment"
    );
    assert!(matches!(
        &run.finished.errors[..],
        [Error::Storage(failure)] if matches!(failure.errors(), [StorageError::JoinTimeout(_)])
    ));
    drop(gate);
})
.await;
    });
}

#[test]
fn an_open_failure_ends_the_trial_early_without_delaying_teardown() {
    crate::testing::runtime(false).block_on(async move {
        local(async {
            let timing = Timing {
                sample: Duration::from_millis(50),
                ..Timing::default()
            };
            let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
                env.storage = Some(Rc::new(|| {
                    Ok(
                        Box::new(|| Err(StorageError::Create(std::io::Error::other("denied"))))
                            as Open,
                    )
                }));
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(30))).await.unwrap();
            let begun = std::time::Instant::now();
            let run = finish(&handle).await;
            assert!(
                begun.elapsed() < secs(5),
                "the open failure ended a 30 s trial"
            );
            assert!(run.finished.result.ran < secs(5));
            assert!(matches!(&run.finished.errors[..], [Error::Storage(_)]));
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

struct Failing;

impl Storage for Failing {
    fn append(&mut self, _line: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("disk full"))
    }

    fn close(self: Box<Self>) -> std::io::Result<()> {
        Err(std::io::Error::other("close failed"))
    }
}

#[test]
fn a_write_failure_ends_the_trial_and_keeps_the_close_error_too() {
    crate::testing::runtime(false).block_on(async move {
        local(async {
            let timing = Timing {
                sample: Duration::from_millis(50),
                ..Timing::default()
            };
            let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
                env.storage = Some(Rc::new(|| {
                    Ok(Box::new(|| Ok(Box::new(Failing) as Box<dyn Storage>)) as Open)
                }));
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(30))).await.unwrap();
            let run = finish(&handle).await;
            assert!(run.finished.result.ran < secs(5));
            match &run.finished.errors[..] {
                [Error::Storage(failure)] => assert!(matches!(
                    failure.errors(),
                    [StorageError::Append(_), StorageError::Close(_)]
                )),
                other => panic!("{other:?}"),
            }
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

/// Opens `recorder` only once `gate` is released, so the setup itself blocks and not just a write.
fn gated_open(
    gate: Receiver<()>,
    recorder: &Recorder,
) -> Rc<dyn Fn() -> Result<Open, StorageError>> {
    let slot = Mutex::new(Some((gate, recorder.clone())));
    Rc::new(move || {
        let parts = slot.lock().unwrap().take();
        Ok(Box::new(move || {
            let (gate, recorder) =
                parts.ok_or_else(|| StorageError::Create(std::io::Error::other("opened twice")))?;
            let _ = gate.recv();
            Ok(Box::new(recorder) as Box<dyn Storage>)
        }) as Open)
    })
}

/// Whether the supervisor has published its end; reading it would hand it to nobody else.
fn ended(handle: &Handle) -> bool {
    while let Ok(event) = handle.events.try_recv() {
        if matches!(event, Event::Finished(_)) {
            return true;
        }
    }
    false
}

/// Advances virtual time by hand until the first worker has been reaped. The supervisor then waits
/// for the writer's join, which runs in real time and keeps the runtime from auto-advancing, so a
/// virtual sleep that was pending then would never wake; a manual advance does not depend on it.
async fn until_reaped(rig: &super::harness::Rig) {
    while !rig.fake.process(0).reaped.get() {
        tokio::time::advance(Duration::from_millis(10)).await;
    }
}

#[test]
fn cancelling_while_samples_setup_is_blocked_stops_the_workload_before_the_setup_ends() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (release, gate) = channel::<()>();
            let recorder = Recorder::default();
            let timing = Timing {
                writer_join: secs(30),
                ..Timing::default()
            };
            let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
                env.storage = Some(gated_open(gate, &recorder));
            });
            let cancel = CancellationToken::new();
            let handle = rig
                .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(1500)).await;
            cancel.cancel();
            until_reaped(&rig).await;
            let process = rig.fake.process(0);
            assert!(
                process.signals.borrow().contains(&GroupSignal::Term) && process.reaped.get(),
                "cancellation waited for the samples setup before stopping the workload"
            );
            assert!(
                !ended(&handle),
                "the end was published before the setup finished"
            );
            assert!(!recorder.closed.load(Ordering::Acquire));

            release.send(()).unwrap();
            let run = finish(&handle).await;
            assert!(matches!(run.finished.errors[..], [Error::Cancelled]));
            assert!(run.finished.teardown.is_ok());
            let ran = run.finished.result.ran;
            assert!(
                ran >= Duration::from_millis(1500) && ran < Duration::from_millis(1600),
                "{ran:?}"
            );
            let samples = recorder.samples();
            assert_eq!(
                samples.len(),
                1,
                "the sample taken during setup was drained"
            );
            assert_eq!(samples[0]["elapsed_ms"], 1000);
            assert!(
                recorder.closed.load(Ordering::Acquire),
                "the file was not closed"
            );
        })
        .await;
    });
}

#[test]
fn blocked_samples_setup_keeps_the_load_schedule_and_the_end_waits_for_the_writer() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (release, gate) = channel::<()>();
            let recorder = Recorder::default();
            let timing = Timing {
                writer_join: secs(30),
                ..Timing::default()
            };
            let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
                env.storage = Some(gated_open(gate, &recorder));
            });
            let trial = spec(Regime::R4, &[0], secs(3));
            let handle = rig.start(trial.clone()).await.unwrap();
            let t0 = tokio::time::Instant::now();
            // Up to the deadline virtual time runs by itself, so the load steps land exactly when
            // due; the teardown after it is driven by hand.
            tokio::time::sleep(secs(3)).await;
            until_reaped(&rig).await;
            let steps: Vec<_> = rig
                .fake
                .process(0)
                .signal_times
                .borrow()
                .iter()
                .filter(|(at, signal)| {
                    at.duration_since(t0) < secs(3)
                        && matches!(signal, GroupSignal::Stop | GroupSignal::Cont)
                })
                .map(|(at, _)| at.duration_since(t0).as_millis())
                .collect();
            let load =
                crate::plan::Load::Steps(togi_machine::LoadSchedule::for_spec(&trial).unwrap());
            let expected: Vec<u128> = crate::schedule::timeline(&[load], secs(3))
                .map(|step| step.at.as_millis())
                .collect();
            assert_eq!(steps, expected);
            assert!(
                !ended(&handle),
                "the end was published before the pending sample was durable"
            );

            release.send(()).unwrap();
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.ran, secs(3));
            assert!(run.finished.errors.is_empty(), "{:?}", run.finished.errors);
            assert!(run.finished.teardown.is_ok());
            let samples = recorder.samples();
            assert_eq!(
                samples.len(),
                1,
                "only the first sample fits behind a blocked setup"
            );
            assert_eq!(samples[0]["elapsed_ms"], 1000);
            assert!(
                recorder.closed.load(Ordering::Acquire),
                "the file was not closed"
            );
        })
        .await;
    });
}

fn open_fails_when_released(
    gate: Receiver<()>,
    entered: Arc<tokio::sync::Notify>,
) -> Rc<dyn Fn() -> Result<Open, StorageError>> {
    let slot = std::cell::RefCell::new(Some(gate));
    Rc::new(move || {
        let gate = slot.borrow_mut().take().unwrap();
        let entered = Arc::clone(&entered);
        Ok(Box::new(move || {
            entered.notify_one();
            let _ = gate.recv();
            Err(StorageError::Create(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected delayed open failure",
            )))
        }) as Open)
    })
}

async fn notified_without_advancing(notify: &tokio::sync::Notify) {
    let _clock = crate::join::ClockHold::new(secs(5));
    tokio::time::timeout(secs(5), notify.notified())
        .await
        .expect("the writer did not report its transition");
}

struct TeardownNotified {
    file: crate::logs::Sink,
    completed: Arc<tokio::sync::Notify>,
}

impl std::io::Write for TeardownNotified {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(&mut self.file, bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.file)
    }
}

impl Drop for TeardownNotified {
    fn drop(&mut self) {
        self.completed.notify_one();
    }
}

#[test]
fn an_open_failure_before_the_deadline_ends_the_trial_when_it_happens() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (release, gate) = channel();
            let entered = Arc::new(tokio::sync::Notify::new());
            let timing = Timing {
                sample: Duration::from_millis(50),
                ..Timing::default()
            };
            let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
                env.storage = Some(open_fails_when_released(gate, Arc::clone(&entered)));
            });
            let failed_at = Duration::from_millis(150);
            let handle = rig
                .start(spec(Regime::R1, &[0], Duration::from_millis(900)))
                .await
                .unwrap();
            notified_without_advancing(&entered).await;
            tokio::time::advance(failed_at).await;
            let clock = crate::join::ClockHold::new(secs(5));
            release.send(()).unwrap();
            let process = rig.fake.process(0);
            while !process.signals.borrow().contains(&GroupSignal::Term) {
                tokio::task::yield_now().await;
            }
            drop(clock);
            until_reaped(&rig).await;
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.ran, failed_at);
            assert!(matches!(
                &run.finished.errors[..],
                [Error::Storage(failure)] if matches!(failure.errors(), [StorageError::Create(_)])
            ));
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn samples_setup_blocked_past_the_join_bound_reports_timeout_after_teardown() {
    crate::testing::runtime(true).block_on(async move { local(async {
    let (release, gate) = channel();
    let entered = Arc::new(tokio::sync::Notify::new());
    let timing = Timing {
        sample: Duration::from_millis(50),
        writer_join: Duration::from_millis(200),
        ..Timing::default()
    };
    let duration = Duration::from_millis(400);
    let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
        env.storage = Some(open_fails_when_released(gate, Arc::clone(&entered)));
    });
    let handle = rig.start(spec(Regime::R1, &[0], duration)).await.unwrap();
    notified_without_advancing(&entered).await;
    tokio::time::sleep(duration).await;
    until_reaped(&rig).await;
    let process = rig.fake.process(0);
    assert!(process.signals.borrow().contains(&GroupSignal::Term));
    assert!(process.reaped.get());
    assert!(!ended(&handle), "the blocked writer was not joined");
    let run = finish(&handle).await;
    release.send(()).unwrap();
    assert_eq!(run.finished.result.ran, duration);
    assert_eq!(run.finished.result.failure, None);
    assert_eq!(run.finished.result.inconclusive, None);
    assert!(run.finished.result.escaped.is_empty());
    assert!(
        matches!(
            &run.finished.errors[..],
            [Error::Storage(failure)]
                if matches!(failure.errors(), [StorageError::JoinTimeout(bound)] if *bound == timing.writer_join)
        ),
        "{:?}",
        run.finished.errors
    );
    assert!(run.finished.teardown.is_ok());
})
.await; });
}

#[test]
fn an_open_failure_during_the_post_teardown_join_keeps_the_original_create_error() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let (release, gate) = channel();
            let entered = Arc::new(tokio::sync::Notify::new());
            let teardown_completed = Arc::new(tokio::sync::Notify::new());
            let timing = Timing {
                sample: Duration::from_millis(50),
                ..Timing::default()
            };
            let duration = Duration::from_millis(400);
            let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
                env.storage = Some(open_fails_when_released(gate, Arc::clone(&entered)));
                let completed = Arc::clone(&teardown_completed);
                env.log_sink = Some(Rc::new(move || {
                    let completed = Arc::clone(&completed);
                    Box::new(move |file| {
                        Box::new(TeardownNotified {
                            file,
                            completed: Arc::clone(&completed),
                        }) as crate::logs::Sink
                    })
                }));
            });
            let handle = rig.start(spec(Regime::R1, &[0], duration)).await.unwrap();
            notified_without_advancing(&entered).await;
            tokio::time::sleep(duration).await;
            until_reaped(&rig).await;
            notified_without_advancing(&teardown_completed).await;
            let process = rig.fake.process(0);
            assert!(process.signals.borrow().contains(&GroupSignal::Term));
            assert!(process.reaped.get());
            assert!(!ended(&handle), "the end preceded the blocked Open result");
            release.send(()).unwrap();
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.ran, duration);
            assert_eq!(run.finished.result.failure, None);
            assert_eq!(run.finished.result.inconclusive, None);
            assert!(run.finished.result.escaped.is_empty());
            assert!(run.finished.teardown.is_ok());
            let [Error::Storage(failure)] = &run.finished.errors[..] else {
                panic!(
                    "the delayed Create was discarded: {:?}",
                    run.finished.errors
                );
            };
            let [StorageError::Create(source)] = failure.errors() else {
                panic!("the original Create was replaced: {:?}", failure.errors());
            };
            assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
            assert_eq!(source.to_string(), "injected delayed open failure");
        })
        .await;
    });
}

#[test]
fn a_writer_that_cannot_start_ends_the_trial_with_a_storage_error() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
                env.storage = Some(Rc::new(|| {
                    Err(StorageError::Spawn(std::io::Error::other(
                        "no threads left",
                    )))
                }));
            });
            let handle = rig.start(spec(Regime::R1, &[0], secs(60))).await.unwrap();
            let run = finish(&handle).await;
            assert!(run.finished.result.ran < secs(1));
            assert!(matches!(
                &run.finished.errors[..],
                [Error::Storage(failure)] if matches!(failure.errors(), [StorageError::Spawn(_)])
            ));
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn a_stuck_writer_join_does_not_block_the_runtime_thread() {
    crate::testing::runtime(true).block_on(async move {
        const BURST: u32 = 1000;
        local(async {
    let (gate, gated) = std::sync::mpsc::channel::<()>();
    let lines = Arc::new(Mutex::new(0));
    let storage = blocked_open(gated, Arc::clone(&lines));
    let timing = Timing {
        writer_join: secs(1),
        ..Timing::default()
    };
    let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
        env.storage = Some(storage);
    });
    let duration = secs(2);
    let handle = rig.start(spec(Regime::R1, &[0], duration)).await.unwrap();
    tokio::time::sleep(duration).await;
    until_reaped(&rig).await;
    assert!(!ended(&handle), "the blocked writer was not joined");
    let turns = Rc::new(std::cell::Cell::new(0_u32));
    let counter = Rc::clone(&turns);
    tokio::task::spawn_local(async move {
        for _ in 0..BURST {
            tokio::task::yield_now().await;
            counter.set(counter.get() + 1);
        }
    })
    .await
    .unwrap();
    assert_eq!(turns.get(), BURST);
    assert!(
        !ended(&handle),
        "the runtime ran other tasks while the join was still pending"
    );
    let run = finish(&handle).await;
    assert!(
        matches!(
            &run.finished.errors[..],
            [Error::Storage(failure)] if matches!(failure.errors(), [StorageError::JoinTimeout(_)])
        ),
        "{:?}",
        run.finished.errors
    );
    drop(gate);
})
.await;
    });
}

#[test]
fn a_log_writer_that_cannot_start_is_refused_with_its_error_the_proof_and_no_workload() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            crate::logs::refusal::arm();
            let partial = rig
                .start(spec(Regime::R1, &[0], secs(5)))
                .await
                .err()
                .unwrap();
            assert!(partial.value.is_some(), "the stop proof is returned");
            assert!(matches!(
                super::harness::crate_error(&partial.error),
                Some(Error::LogWriter(error)) if error.to_string() == "no threads left"
            ));
            assert_eq!(rig.fake.process_count(), 0);
            assert!(rig.fake.launches().is_empty());
        })
        .await;
    });
}

#[test]
fn the_trial_keeps_the_highest_tctl_it_saw_and_leaves_out_a_ccd_that_reads_as_no_number() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let landed = Arc::new(Landed::default());
            let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
                env.storage = Some(landed.storage());
            });
            let tick = std::cell::Cell::new(0);
            let before_tick = Arc::clone(&landed);
            rig.fake.on_threads(move |_| {
                tick.set(tick.get() + 1);
                before_tick.wait_for(tick.get() - 1);
            });
            write(&rig, "hwmon/hwmon0/name", "k10temp\n");
            write(&rig, "hwmon/hwmon0/temp1_label", "Tctl\n");
            write(&rig, "hwmon/hwmon0/temp1_input", "71000\n");
            write(&rig, "hwmon/hwmon0/temp2_label", "Tccd1\n");
            write(&rig, "hwmon/hwmon0/temp2_input", "65000\n");
            write(&rig, "hwmon/hwmon0/temp3_label", "Tccd2\n");
            write(&rig, "hwmon/hwmon0/temp3_input", "not a temperature\n");
            let handle = rig.start(spec(Regime::R1, &[2], secs(3))).await.unwrap();
            let dir = rig.dir.path().to_owned();
            let memory = rig.memory.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_millis(1500)).await;
                memory
                    .write_file(dir.join("hwmon/hwmon0/temp1_input"), "72000\n", None)
                    .unwrap();
            });
            let run = finish(&handle).await;
            assert!(run.finished.errors.is_empty(), "{:?}", run.finished.errors);
            assert_eq!(run.finished.result.tctl_max_c, Some(72));
            let samples: Vec<_> = landed.samples();
            assert_eq!(
                samples
                    .iter()
                    .map(|sample| sample.tctl_c)
                    .collect::<Vec<_>>(),
                [Some(71), Some(72)]
            );
            for sample in &samples {
                assert_eq!(
                    sample.tccd_c,
                    Some(std::collections::BTreeMap::from([("Tccd1".to_owned(), 65)]))
                );
            }
        })
        .await;
    });
}
