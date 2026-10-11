//! Conditions samples through the supervisor: sensors, the writer and its failure modes.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use togi_machine::{CancellationToken, Regime, Signal};

use super::harness::{core, finish, local, rig, rig_edit, spec};
use crate::error::Error;
use crate::error::StorageError;
use crate::os::GroupSignal;
use crate::os::fake::{Launch, ProcCtx, script};
use crate::samples::{Open, Storage};
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
) -> Rc<dyn Fn() -> Open> {
    let slot = Mutex::new(Some((gate, lines)));
    Rc::new(move || {
        let parts = slot.lock().unwrap().take();
        Box::new(move || {
            let (gate, lines) =
                parts.ok_or_else(|| StorageError::Create(std::io::Error::other("reused")))?;
            Ok(Box::new(Blocking { gate, lines }) as Box<dyn Storage>)
        })
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
                Box::new(|| Err(StorageError::Create(std::io::Error::other("denied"))))
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
                Box::new(|| Ok(Box::new(Failing) as Box<dyn Storage>))
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

#[tokio::test(start_paused = true)]
async fn cancelling_while_storage_setup_blocks_stops_the_workload_and_joins_the_writer() {
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
        let cancel = CancellationToken::new();
        let handle = rig
            .start_with(&cancel, spec(Regime::R1, &[0], secs(60)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel.cancel();
        gate.send(()).unwrap();
        let run = finish(&handle).await;
        assert!(matches!(
            run.finished.errors.first(),
            Some(Error::Cancelled)
        ));
        assert!(run.finished.teardown.is_ok());
        assert_eq!(rig.fake.process(0).signals.borrow()[0], GroupSignal::Cont);
        let _ = (
            Signal::Stall,
            script(|_: ProcCtx| async {}),
            Launch::Fail(std::io::Error::other("x")),
        );
    })
    .await;
}
