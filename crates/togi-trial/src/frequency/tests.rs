use std::path::Path;
use std::sync::Arc;

use togi_journal::fs::{Fault, Fs as _, MemFs, Op};
use togi_machine::{CpuFrequency as _, CpuId, FrequencyPolicy, FrequencyPolicyId};

use super::fake::Fake;
use super::{Limits, PENDING};

fn policy(cpus: &[u16]) -> FrequencyPolicy {
    FrequencyPolicy {
        id: FrequencyPolicyId::new(0),
        related_cpus: cpus.iter().copied().map(CpuId::from).collect(),
        cpuinfo_min_khz: 400_000,
        scaling_min_khz: 400_000,
        scaling_max_khz: 5_700_000,
        available_khz: None,
    }
}

fn fixture(policies: Vec<FrequencyPolicy>) -> (Fake, Arc<MemFs>, Limits) {
    let fs = Arc::new(MemFs::new());
    fs.create_dir(Path::new("/trials")).unwrap();
    let machine = Fake::new(policies);
    let limits = Limits::new(
        Arc::new(machine.clone()),
        "/trials".into(),
        Some(fs.clone()),
    );
    (machine, fs, limits)
}

#[test]
fn cap_is_durable_before_write_and_restores_original_readback() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    fs.record_history();
    let observed = fs.clone();
    machine.before_write(move || {
        assert!(
            observed
                .contents(Path::new("/trials").join(PENDING))
                .is_some()
        );
        assert!(observed.log().iter().any(|(op, _)| *op == Op::SyncDir));
    });
    let mut report = Vec::new();
    limits
        .cap(
            &[CpuId::from(0), CpuId::from(16)],
            &[CpuId::from(1)],
            &mut report,
        )
        .unwrap();
    assert_eq!(
        machine.scaling_max_khz(FrequencyPolicyId::new(0)).unwrap(),
        400_000
    );
    limits.restore(&mut report).unwrap();
    assert_eq!(
        machine.scaling_max_khz(FrequencyPolicyId::new(0)).unwrap(),
        5_700_000
    );
    assert_eq!(fs.contents(Path::new("/trials").join(PENDING)), None);
    assert!(
        report
            .iter()
            .any(|line| line.contains("saved scaling_max_freq 5700000"))
    );
    assert!(report.iter().any(|line| line.contains("cap readback")));
    assert!(report.iter().any(|line| line.contains("restore readback")));
}

#[test]
fn shared_worker_policy_is_refused_before_any_write() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 1, 16, 17])]);
    assert!(
        limits
            .cap(&[CpuId::from(0)], &[CpuId::from(1)], &mut Vec::new())
            .is_err()
    );
    assert_eq!(machine.writes(), Vec::new());
    assert_eq!(fs.contents(Path::new("/trials").join(PENDING)), None);
}

#[test]
fn host_smt_policy_is_allowed_only_when_worker_set_excludes_that_core() {
    let (machine, _, limits) = fixture(vec![policy(&[0, 16])]);
    limits
        .cap(
            &[CpuId::from(0), CpuId::from(16)],
            &[CpuId::from(1), CpuId::from(17)],
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(machine.writes(), vec![(FrequencyPolicyId::new(0), 400_000)]);
}

#[test]
fn same_boot_new_runner_recovers_original_values() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).unwrap();
    drop(limits);
    let replacement = Limits::new(Arc::new(machine.clone()), "/trials".into(), Some(fs));
    replacement.restore(&mut Vec::new()).unwrap();
    assert_eq!(
        machine.writes(),
        vec![
            (FrequencyPolicyId::new(0), 400_000),
            (FrequencyPolicyId::new(0), 5_700_000)
        ]
    );
}

#[test]
fn previous_boot_discards_intent_without_restoring_obsolete_values() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).unwrap();
    machine.reboot();
    let mut report = Vec::new();
    limits.restore(&mut report).unwrap();
    assert_eq!(machine.writes(), vec![(FrequencyPolicyId::new(0), 400_000)]);
    assert_eq!(fs.contents(Path::new("/trials").join(PENDING)), None);
    assert!(report[0].contains("previous boot"));
}

#[test]
fn cap_write_refusal_keeps_original_recoverable() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    machine.refuse_write(true);
    assert!(limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).is_err());
    assert!(fs.contents(Path::new("/trials").join(PENDING)).is_some());
    machine.refuse_write(false);
    limits.restore(&mut Vec::new()).unwrap();
    assert_eq!(
        machine.scaling_max_khz(FrequencyPolicyId::new(0)).unwrap(),
        5_700_000
    );
}

#[test]
fn wrong_cap_readback_keeps_original_recoverable() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    machine.wrong_readback(true);
    assert!(limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).is_err());
    assert!(fs.contents(Path::new("/trials").join(PENDING)).is_some());
    machine.wrong_readback(false);
    limits.restore(&mut Vec::new()).unwrap();
    assert_eq!(
        machine.scaling_max_khz(FrequencyPolicyId::new(0)).unwrap(),
        5_700_000
    );
}

#[test]
fn failed_restore_readback_keeps_intent_until_retry_confirms_restoration() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).unwrap();
    machine.wrong_readback(true);
    assert!(limits.restore(&mut Vec::new()).is_err());
    assert!(fs.contents(Path::new("/trials").join(PENDING)).is_some());
    machine.wrong_readback(false);
    limits.restore(&mut Vec::new()).unwrap();
    assert_eq!(fs.contents(Path::new("/trials").join(PENDING)), None);
}

#[test]
fn refused_restore_write_keeps_intent_until_retry() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).unwrap();
    machine.refuse_write(true);
    assert!(limits.restore(&mut Vec::new()).is_err());
    assert!(fs.contents(Path::new("/trials").join(PENDING)).is_some());
    machine.refuse_write(false);
    limits.restore(&mut Vec::new()).unwrap();
    assert_eq!(
        machine.scaling_max_khz(FrequencyPolicyId::new(0)).unwrap(),
        5_700_000
    );
}
#[test_case::test_case(Op::Sync; "file sync")]
#[test_case::test_case(Op::Rename; "publication")]
#[test_case::test_case(Op::SyncDir; "directory sync")]
fn failed_intent_durability_never_writes_hardware(operation: Op) {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    fs.inject(Fault::before(operation, 1));
    assert!(limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).is_err());
    assert_eq!(machine.writes(), Vec::new());
}

#[test]
fn supported_minimum_respects_scaling_minimum() {
    let mut selected = policy(&[0, 16]);
    selected.scaling_min_khz = 1_000_000;
    selected.available_khz = Some(vec![400_000, 800_000, 1_200_000, 2_400_000]);
    let (machine, _, limits) = fixture(vec![selected]);
    limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).unwrap();
    assert_eq!(
        machine.writes(),
        vec![(FrequencyPolicyId::new(0), 1_200_000)]
    );
}

#[test]
fn missing_policy_is_refused() {
    let (machine, _, limits) = fixture(vec![policy(&[0, 16])]);
    assert!(limits.cap(&[CpuId::from(2)], &[], &mut Vec::new()).is_err());
    assert_eq!(machine.writes(), Vec::new());
}

#[test]
fn discovery_failure_is_refused() {
    let (machine, _, limits) = fixture(vec![policy(&[0, 16])]);
    machine.fail_discovery();
    assert!(limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).is_err());
    assert_eq!(machine.writes(), Vec::new());
}
