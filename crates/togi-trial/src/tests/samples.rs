//! Conditions samples through the supervisor: sensors, the writer and its failure modes.

use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use togi_machine::{CancellationToken, Regime};

use super::harness::{core, finish, local, rig, rig_edit, spec};
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
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[tokio::test(start_paused = true)]
async fn each_tick_persists_a_conditions_sample_with_sensors_workers_and_power() {
    local(async {
        let rig = rig();
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
                    cpu: 2,
                });
            });
        }
        let dir = rig.dir.path().to_owned();
        for (at, energy) in [(500, "10000000"), (1500, "20000000")] {
            let dir = dir.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(Duration::from_millis(at)).await;
                std::fs::write(dir.join("powercap/intel-rapl:0/energy_uj"), energy).unwrap();
            });
        }
        let run = finish(&handle).await;
        assert!(run.finished.errors.is_empty(), "{:?}", run.finished.errors);
        assert_eq!(run.finished.result.tctl_max_c, Some(62));
        let samples: Vec<_> = togi_machine::read_samples(&dir.join("trials/t1")).collect();
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
            assert_eq!(sample.core_mhz.len(), 1);
            assert_eq!(sample.package_power_w, Some(10.0));
            assert_eq!(
                sample.worker_cpu_ms.get(core(2)),
                Some(1000 * (i64::try_from(index).unwrap() + 1))
            );
            assert!(sample.pm_table.is_none());
        }
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn missing_sensors_are_omitted_not_zeroed() {
    local(async {
        let rig = rig();
        let handle = rig.start(spec(Regime::R1, &[2], secs(2))).await.unwrap();
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.tctl_max_c, None);
        let samples: Vec<_> =
            togi_machine::read_samples(&rig.dir.path().join("trials/t1")).collect();
        assert_eq!(samples.len(), 1);
        let sample = &samples[0];
        assert!(sample.tctl_c.is_none() && sample.tccd_c.is_none());
        assert!(sample.core_mhz.is_empty() && sample.package_power_w.is_none());
    })
    .await;
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

#[tokio::test(start_paused = true)]
async fn slow_storage_neither_delays_load_steps_nor_the_trial_and_the_join_is_bounded() {
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
        let expected: Vec<u128> = crate::schedule::timeline(&load, secs(3))
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
}

#[tokio::test]
async fn an_open_failure_ends_the_trial_early_without_delaying_teardown() {
    local(async {
        let timing = Timing {
            sample: Duration::from_millis(50),
            ..Timing::default()
        };
        let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
            env.storage = Some(Rc::new(|| {
                Ok(Box::new(|| Err(StorageError::Create(std::io::Error::other("denied")))) as Open)
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

#[tokio::test]
async fn a_write_failure_ends_the_trial_and_keeps_the_close_error_too() {
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

#[tokio::test(start_paused = true)]
async fn cancelling_while_samples_setup_is_blocked_stops_the_workload_before_the_setup_ends() {
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
}

#[tokio::test(start_paused = true)]
async fn blocked_samples_setup_keeps_the_load_schedule_and_the_end_waits_for_the_writer() {
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
        let load = crate::plan::Load::Steps(togi_machine::LoadSchedule::for_spec(&trial).unwrap());
        let expected: Vec<u128> = crate::schedule::timeline(&load, secs(3))
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
}

fn open_fails_after(delay: Duration) -> Rc<dyn Fn() -> Result<Open, StorageError>> {
    Rc::new(move || {
        Ok(Box::new(move || {
            std::thread::sleep(delay);
            Err(StorageError::Create(std::io::Error::other(
                "injected open failure",
            )))
        }) as Open)
    })
}

#[tokio::test]
async fn an_open_failure_before_the_deadline_ends_the_trial_when_it_happens() {
    local(async {
        let timing = Timing {
            sample: Duration::from_millis(50),
            ..Timing::default()
        };
        let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
            env.storage = Some(open_fails_after(Duration::from_millis(150)));
        });
        let handle = rig
            .start(spec(Regime::R1, &[0], Duration::from_millis(900)))
            .await
            .unwrap();
        let run = finish(&handle).await;
        let ran = run.finished.result.ran;
        assert!(
            ran >= Duration::from_millis(100) && ran < Duration::from_millis(900),
            "{ran:?}"
        );
        assert!(matches!(
            &run.finished.errors[..],
            [Error::Storage(failure)] if matches!(failure.errors(), [StorageError::Create(_)])
        ));
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

#[tokio::test]
async fn an_open_failure_after_the_deadline_neither_delays_teardown_nor_is_lost() {
    local(async {
        let timing = Timing {
            sample: Duration::from_millis(50),
            writer_join: Duration::from_millis(200),
            ..Timing::default()
        };
        let duration = Duration::from_millis(400);
        let rig = rig_edit(TestBackend::new("/bin/true"), timing, |env| {
            env.storage = Some(open_fails_after(Duration::from_millis(1500)));
        });
        let handle = rig.start(spec(Regime::R1, &[0], duration)).await.unwrap();
        let begun = tokio::time::Instant::now();
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.ran, duration);
        let terminated = rig
            .fake
            .process(0)
            .signal_times
            .borrow()
            .iter()
            .find(|(_, signal)| *signal == GroupSignal::Term)
            .map(|(at, _)| at.duration_since(begun))
            .unwrap();
        assert!(
            terminated < duration + Duration::from_millis(250),
            "sample setup delayed the teardown past the deadline: {terminated:?}"
        );
        assert!(matches!(
            &run.finished.errors[..],
            [Error::Storage(failure)] if matches!(failure.errors(), [StorageError::JoinTimeout(_)])
        ));
        assert!(run.finished.teardown.is_ok());
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_writer_that_cannot_start_ends_the_trial_with_a_storage_error() {
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
}

#[tokio::test]
async fn a_stuck_writer_join_does_not_block_the_runtime_thread() {
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
        let ticks = Rc::new(std::cell::Cell::new(0_u32));
        let counter = Rc::clone(&ticks);
        let ticker = tokio::task::spawn_local(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                counter.set(counter.get() + 1);
            }
        });
        let handle = rig.start(spec(Regime::R1, &[0], secs(2))).await.unwrap();
        let wall = std::time::Instant::now();
        let run = finish(&handle).await;
        ticker.abort();
        let elapsed = wall.elapsed();
        assert!(elapsed >= Duration::from_millis(2900), "{elapsed:?}");
        let expected = u32::try_from(elapsed.as_millis() / 20).unwrap();
        assert!(
            ticks.get() * 4 >= expected * 3,
            "the runtime kept running during the join: {} of {expected} ticks",
            ticks.get()
        );
        assert!(matches!(&run.finished.errors[..], [Error::Storage(_)]));
        drop(gate);
    })
    .await;
}
