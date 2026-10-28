use std::io;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use togi_machine::{
    CancellationToken, ControllerMode, CpuFrequency as _, CpuId, FrequencyPolicy,
    FrequencyPolicyId, Purpose, Regime, Signal,
};

use super::harness::{core, finish, local, printing, rig_edit, spec, topology};
use crate::controller::{Controller, Moved, Recording};
use crate::frequency::{Limits, fake::Fake};
use crate::os::fake::Launch;
use crate::runtime::{Command, serve_detached};
use crate::testing::TestBackend;
use crate::timing::Timing;

fn policies() -> Vec<FrequencyPolicy> {
    topology()
        .iter()
        .map(|info| FrequencyPolicy {
            id: FrequencyPolicyId::new(u32::from(info.core.get())),
            related_cpus: info.cpus.clone(),
            cpuinfo_min_khz: 400_000,
            scaling_min_khz: 400_000,
            scaling_max_khz: 5_700_000,
            available_khz: None,
        })
        .collect()
}

fn rig(machine: &Fake) -> super::harness::Rig {
    rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
        env.controller = Arc::new(Recording::default());
        env.frequency = Arc::new(Limits::new(
            Arc::new(machine.clone()),
            env.dir.clone(),
            env.preparer.fs.clone(),
            env.durability,
        ));
    })
}

fn managed() -> togi_machine::TrialSpec {
    let mut trial = spec(Regime::R1, &[2], Duration::from_secs(5));
    trial.controller = ControllerMode::Managed {
        rotation: 0,
        purpose: Purpose::Isolated,
    };
    trial
}

#[test]
fn managed_mapping_caps_idle_ccd_before_launch_and_restores_after_teardown() {
    crate::testing::runtime(true).block_on(local(async {
        let machine = Fake::new(policies());
        let rig = rig(&machine);
        let handle = rig.start(managed()).await.unwrap();
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(8)).unwrap(),
            400_000
        );
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.inconclusive, None);
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(8)).unwrap(),
            5_700_000
        );
        assert!(
            run.progress()
                .iter()
                .any(|line| line.contains("frequency cap readback"))
        );
        assert!(
            run.progress()
                .iter()
                .any(|line| line.contains("frequency restore readback"))
        );
    }));
}

#[test]
fn refused_cap_withholds_pass_without_hiding_computation_failure() {
    crate::testing::runtime(true).block_on(local(async {
        let machine = Fake::new(policies());
        machine.refuse_write(true);
        let rig = rig(&machine);
        rig.fake.on_launch(printing("COMPUTE ERROR\n"));
        let handle = rig.start(managed()).await.unwrap();
        let run = finish(&handle).await;
        assert!(run.finished.result.inconclusive.is_some());
        let failure = run.finished.result.failure.unwrap();
        assert_eq!(
            (failure.signal, failure.core),
            (Signal::ComputationError, core(2))
        );
    }));
}

#[test]
fn wrong_cap_readback_withholds_pass_and_restoration_retries_later() {
    crate::testing::runtime(true).block_on(local(async {
        let machine = Fake::new(policies());
        machine.wrong_readback(true);
        let rig = rig(&machine);
        let handle = rig.start(managed()).await.unwrap();
        let run = finish(&handle).await;
        assert!(run.finished.result.inconclusive.is_some());
        machine.wrong_readback(false);
        let (reply, answer) = mpsc::sync_channel(1);
        serve_detached(
            Rc::clone(&rig.env),
            Command::Sweep {
                cancel: CancellationToken::new(),
                reply,
            },
        )
        .await;
        let swept = answer.recv().unwrap().unwrap();
        assert!(swept.detail.contains("frequency restore readback"));
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(8)).unwrap(),
            5_700_000
        );
    }));
}

#[test]
fn start_rollback_restores_caps() {
    crate::testing::runtime(true).block_on(local(async {
        let machine = Fake::new(policies());
        let rig = rig(&machine);
        rig.fake
            .on_launch(|_, _| Launch::Fail(io::Error::other("launch refused")));
        let failed = rig.start(managed()).await.err().unwrap();
        assert!(failed.value.is_some());
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(8)).unwrap(),
            5_700_000
        );
        assert!(togi_machine::error_chain(&failed.error).contains("frequency restore readback"));
    }));
}

struct Incomplete;

impl Controller for Incomplete {
    fn confine(&self, _: &[CpuId]) -> Option<Moved> {
        Some(Moved {
            threads: 1,
            failed: 1,
            incomplete: false,
        })
    }

    fn restore(&self) -> Option<Moved> {
        Some(Moved::default())
    }
}

#[test]
fn incomplete_managed_affinity_withholds_pass() {
    crate::testing::runtime(true).block_on(local(async {
        let machine = Fake::new(policies());
        let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
            env.controller = Arc::new(Incomplete);
            env.frequency = Arc::new(Limits::new(
                Arc::new(machine.clone()),
                env.dir.clone(),
                env.preparer.fs.clone(),
                env.durability,
            ));
        });
        let handle = rig.start(managed()).await.unwrap();
        let run = finish(&handle).await;
        assert!(
            run.finished
                .result
                .inconclusive
                .as_deref()
                .unwrap()
                .contains("did not confine")
        );
    }));
}

#[test]
fn single_ccd_excludes_only_host_and_keeps_other_workers_uncapped() {
    crate::testing::runtime(true).block_on(local(async {
        let machine = Fake::new(policies());
        let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
            env.topology.retain(|info| info.core.get() < 8);
            env.controller = Arc::new(Recording::default());
            env.frequency = Arc::new(Limits::new(
                Arc::new(machine.clone()),
                env.dir.clone(),
                env.preparer.fs.clone(),
                env.durability,
            ));
        });
        let mut trial = spec(
            Regime::R7,
            &[0, 1, 2, 3, 4, 5, 6, 7],
            Duration::from_secs(5),
        );
        trial.controller = ControllerMode::Managed {
            rotation: 2,
            purpose: Purpose::Exact,
        };
        let handle = rig.start(trial).await.unwrap();
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(2)).unwrap(),
            400_000
        );
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(1)).unwrap(),
            5_700_000
        );
        let run = finish(&handle).await;
        assert_eq!(
            run.finished.result.excluded,
            [core(2)].into_iter().collect()
        );
        assert_eq!(run.finished.result.inconclusive, None);
    }));
}

#[test]
fn single_ccd_shared_policy_with_another_worker_is_refused() {
    crate::testing::runtime(true).block_on(local(async {
        let mut shared = policies();
        shared[2]
            .related_cpus
            .extend([CpuId::from(1), CpuId::from(33)]);
        let machine = Fake::new(shared);
        let rig = rig_edit(TestBackend::new("/bin/true"), Timing::default(), |env| {
            env.topology.retain(|info| info.core.get() < 8);
            env.controller = Arc::new(Recording::default());
            env.frequency = Arc::new(Limits::new(
                Arc::new(machine.clone()),
                env.dir.clone(),
                env.preparer.fs.clone(),
                env.durability,
            ));
        });
        let mut trial = spec(
            Regime::R7,
            &[0, 1, 2, 3, 4, 5, 6, 7],
            Duration::from_secs(5),
        );
        trial.controller = ControllerMode::Managed {
            rotation: 2,
            purpose: Purpose::Exact,
        };
        let handle = rig.start(trial).await.unwrap();
        let run = finish(&handle).await;
        assert!(run.finished.result.inconclusive.is_some());
        assert_eq!(machine.writes(), Vec::new());
    }));
}

/// Every core of both CCDs is tested, so the controller takes an idle core uncapped.
fn uncapped() -> togi_machine::TrialSpec {
    let mut trial = spec(Regime::R7, &[2, 10], Duration::from_secs(5));
    trial.controller = ControllerMode::Managed {
        rotation: 0,
        purpose: Purpose::Exact,
    };
    trial
}

#[test]
fn an_uncapped_trial_recovers_a_failed_restore_first_or_is_refused() {
    crate::testing::runtime(true).block_on(local(async {
        let machine = Fake::new(policies());
        let rig = rig(&machine);
        let handle = rig.start(managed()).await.unwrap();
        machine.refuse_write(true);
        let run = finish(&handle).await;
        assert!(run.finished.result.inconclusive.is_some());
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(8)).unwrap(),
            400_000,
            "the failed restore left the idle CCD capped"
        );

        let refused = rig.start(uncapped()).await.err().unwrap();
        assert!(togi_machine::error_chain(&refused.error).contains("restore saved limits"));
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(8)).unwrap(),
            400_000
        );

        machine.refuse_write(false);
        let handle = rig.start(uncapped()).await.unwrap();
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(8)).unwrap(),
            5_700_000,
            "the stale cap is lifted before the trial runs"
        );
        let run = finish(&handle).await;
        assert_eq!(run.finished.result.inconclusive, None);
    }));
}
