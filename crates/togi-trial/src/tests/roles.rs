//! Trial roles on the fake seams: R8's light core beside heavy neighbours of another backend, and
//! R9's every-thread phases around the CPU that stays active.

use std::sync::Arc;
use std::time::Duration;

use togi_machine::{CpuId, Signal};

use super::harness::{all_threads_spec, core, finish, local, neighbours_spec, rig, rig_edit};
use crate::os::GroupSignal;
use crate::os::fake::{Launch, ProcCtx, script};
use crate::runner::Backends;
use crate::testing::TestBackend;
use crate::timing::Timing;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// A light y-cruncher backend and a heavy mprime one. Each refuses, by panicking, the line the
/// other one's instances print, so an output classified by the wrong backend ends in an error.
fn mixed_backends(env: &mut crate::supervisor::Env<crate::os::fake::Fake>) {
    let light = TestBackend {
        kind: togi_machine::Backend::Ycruncher,
        panic_on: Some("ok heavy"),
        ..TestBackend::new("/bin/light")
    };
    let heavy = TestBackend {
        kind: togi_machine::Backend::Mprime,
        panic_on: Some("ok light"),
        ..TestBackend::new("/bin/heavy")
    };
    env.backends = Backends::new().with(Arc::new(light)).with(Arc::new(heavy));
}

#[test]
fn r8_launches_a_light_and_a_heavy_backend_together_and_classifies_each_by_its_own() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig_edit(
                TestBackend::new("/bin/true"),
                Timing::default(),
                mixed_backends,
            );
            rig.fake.on_launch(|index, _| {
                let line = if index == 0 {
                    "ok light\n"
                } else {
                    "ok heavy\n"
                };
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    ctx.print(line).await;
                    ctx.until_killed().await;
                }))
            });
            // The y-cruncher light workload on core 0 beside mprime R2 on the rest of its CCD.
            let handle = rig.start(neighbours_spec(1, 0, 0, secs(5))).await.unwrap();
            let programs: Vec<_> = rig
                .fake
                .launches()
                .iter()
                .map(|launch| {
                    launch
                        .argv
                        .iter()
                        .any(|argument| argument.to_string_lossy().ends_with("/bin/light"))
                })
                .collect();
            assert_eq!(programs.len(), 8);
            assert!(programs[0]);
            assert!(programs[1..].iter().all(|light| !light));
            assert_eq!(
                handle
                    .started
                    .instances
                    .iter()
                    .map(|instance| (instance.core, instance.cpus.clone()))
                    .collect::<Vec<_>>(),
                (0..8u8)
                    .map(|n| (core(n), vec![CpuId::from(u16::from(n))]))
                    .collect::<Vec<_>>()
            );
            let run = finish(&handle).await;
            assert!(run.finished.errors.is_empty(), "{:?}", run.finished.errors);
            assert_eq!(run.finished.result.failure, None);
            let progress = run.progress();
            assert!(progress.contains(&"core 00 ok light"));
            assert!(progress.contains(&"core 07 ok heavy"));
        })
        .await;
    });
}

#[test]
fn a_role_whose_backend_is_not_configured_refuses_the_trial_before_anything_launches() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let partial = rig
                .start(neighbours_spec(1, 0, 0, secs(5)))
                .await
                .err()
                .unwrap();
            assert!(matches!(partial.error, togi_machine::Error::BackendMissing));
            assert_eq!(rig.fake.process_count(), 0);
        })
        .await;
    });
}

#[test]
fn a_neighbour_that_fails_is_attributed_to_its_own_core() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            rig.fake.on_launch(|index, _| {
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    if index == 3 {
                        ctx.print("COMPUTE ERROR\n").await;
                    }
                    ctx.until_killed().await;
                }))
            });
            let handle = rig.start(neighbours_spec(0, 0, 0, secs(60))).await.unwrap();
            let run = finish(&handle).await;
            let failure = run.finished.result.failure.unwrap();
            assert_eq!(
                (failure.signal, failure.core),
                (Signal::ComputationError, core(3))
            );
            assert_eq!(run.signals(), [(core(3), Signal::ComputationError)]);
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn an_all_threads_cpu_that_fails_is_attributed_to_its_core_whichever_sibling_it_is() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            // Instances run in core then CPU order, so index 17 is the second CPU of core 8.
            rig.fake.on_launch(|index, _| {
                Launch::Run(script(move |mut ctx: ProcCtx| async move {
                    if index == 17 {
                        ctx.print("COMPUTE ERROR\n").await;
                    }
                    ctx.until_killed().await;
                }))
            });
            let handle = rig.start(all_threads_spec(0, 0, secs(60))).await.unwrap();
            assert_eq!(handle.started.instances.len(), 32);
            assert_eq!(handle.started.instances[17].cpus, [CpuId::from(40)]);
            assert_eq!(handle.started.instances[16].scope, "togi-trial-t1-cpu08");
            assert_eq!(handle.started.instances[17].scope, "togi-trial-t1-cpu40");
            let run = finish(&handle).await;
            let failure = run.finished.result.failure.unwrap();
            assert_eq!(
                (failure.signal, failure.core),
                (Signal::ComputationError, core(8))
            );
            assert_eq!(
                failure.pid.map(std::num::NonZeroU32::get),
                Some(handle.started.instances[17].pid)
            );
            assert_ne!(
                failure.pid.map(std::num::NonZeroU32::get),
                Some(handle.started.instances[16].pid)
            );
            let reported: Vec<_> = run
                .events
                .iter()
                .filter_map(|event| {
                    if let crate::supervisor::Event::Signal { pid, .. } = event {
                        Some(*pid)
                    } else {
                        None
                    }
                })
                .collect();
            assert_eq!(reported, vec![Some(handle.started.instances[17].pid)]);
            assert!(run.finished.teardown.is_ok());
        })
        .await;
    });
}

#[test]
fn an_all_threads_exit_keeps_the_exact_sibling_instance_pid() {
    crate::testing::runtime(true).block_on(local(async {
        let rig = rig();
        rig.fake.on_launch(|index, _| {
            Launch::Run(script(move |mut ctx: ProcCtx| async move {
                if index == 17 {
                    tokio::time::sleep(secs(1)).await;
                    ctx.exit(crate::os::ExitInfo::Signal(11));
                } else {
                    ctx.until_killed().await;
                }
            }))
        });
        let handle = rig.start(all_threads_spec(0, 0, secs(60))).await.unwrap();
        let run = finish(&handle).await;
        let failure = run.finished.result.failure.unwrap();
        assert_eq!(
            (
                failure.signal,
                failure.core,
                failure.pid.map(std::num::NonZeroU32::get)
            ),
            (
                Signal::UnexpectedExit,
                core(8),
                Some(handle.started.instances[17].pid)
            ),
        );
        assert_ne!(
            failure.pid.map(std::num::NonZeroU32::get),
            Some(handle.started.instances[16].pid)
        );
        let reported: Vec<_> = run
            .events
            .iter()
            .filter_map(|event| {
                if let crate::supervisor::Event::Signal { pid, .. } = event {
                    Some(*pid)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(reported, vec![Some(handle.started.instances[17].pid)]);
        assert!(run.finished.teardown.is_ok());
    }));
}

#[test]
fn r8_random_load_stops_only_the_light_core_and_leaves_the_neighbours_running() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            // The mprime R3 workload with random steps on core 0, mprime R2 beside it.
            let spec = neighbours_spec(3, 0, 0, secs(20));
            let togi_machine::PhaseSchedule::Steps { load } = spec.roles[0].schedule else {
                panic!("the light role has no steps");
            };
            let handle = rig.start(spec).await.unwrap();
            let run = finish(&handle).await;
            let expected = load.toggles(secs(20));
            assert!(expected.stops > 0 && expected.conts > 0);
            assert_eq!(
                (run.finished.result.stops, run.finished.result.conts),
                (expected.stops, expected.conts)
            );
            assert_eq!(handle.started.schedule, Some(load));
            let light = rig.fake.process(0).signals.borrow().clone();
            assert_eq!(light[0], GroupSignal::Stop);
            assert_eq!(light[1], GroupSignal::Cont);
            for index in 1..8 {
                assert_eq!(
                    rig.fake.process(index).signals.borrow()[..],
                    [GroupSignal::Cont, GroupSignal::Term, GroupSignal::Kill],
                    "neighbour {index}"
                );
            }
        })
        .await;
    });
}

#[test]
fn r9_stops_every_cpu_but_the_serial_one_in_alternate_ten_second_phases() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let t0 = tokio::time::Instant::now();
            let handle = rig.start(all_threads_spec(0, 0, secs(35))).await.unwrap();
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.failure, None);
            let toggles = |index: usize| -> Vec<(u128, GroupSignal)> {
                rig.fake
                    .process(index)
                    .signal_times
                    .borrow()
                    .iter()
                    .filter(|(at, signal)| {
                        at.duration_since(t0) < secs(35)
                            && matches!(signal, GroupSignal::Stop | GroupSignal::Cont)
                    })
                    .map(|(at, signal)| (at.duration_since(t0).as_millis(), *signal))
                    .collect()
            };
            // Instance 0 is CPU 0, the serial one; instance 1 is its SMT sibling and 2 is core 1.
            assert!(toggles(0).is_empty());
            for index in 1..32 {
                let seen = toggles(index);
                let phases = [
                    (10_000, GroupSignal::Stop),
                    (20_000, GroupSignal::Cont),
                    (30_000, GroupSignal::Stop),
                ];
                assert_eq!(seen.len(), phases.len(), "instance {index}: {seen:?}");
                for ((at, signal), (planned, wanted)) in seen.iter().zip(phases) {
                    assert_eq!(*signal, wanted, "instance {index}");
                    assert!(
                        at.abs_diff(planned) < 1_000,
                        "instance {index}: {signal:?} at {at} ms, planned {planned} ms"
                    );
                }
            }
        })
        .await;
    });
}

#[test]
fn an_r9_trial_names_each_logical_cpu_apart_from_its_sibling() {
    crate::testing::runtime(true).block_on(async move {
        local(async {
            let rig = rig();
            let handle = rig.start(all_threads_spec(0, 9, secs(2))).await.unwrap();
            assert_eq!(handle.started.instances.len(), 32);
            let scopes: std::collections::BTreeSet<_> = handle
                .started
                .instances
                .iter()
                .map(|instance| instance.scope.clone())
                .collect();
            assert_eq!(scopes.len(), 32);
            assert!(scopes.contains("togi-trial-t1-cpu09"));
            assert!(scopes.contains("togi-trial-t1-cpu41"));
            let run = finish(&handle).await;
            assert_eq!(run.finished.result.failure, None);
        })
        .await;
    });
}
