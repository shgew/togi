use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use test_case::test_case;
use togi_journal::fs::{Fault, Fs as _, MemFs, Op};
use togi_machine::{
    CancellationToken, CpuFrequency as _, CpuId, FrequencyPolicy, FrequencyPolicyId, error_chain,
};

use super::fake::{Apply, Fake};
use super::{Error, Limits, PENDING, READBACK_DEADLINE, READBACK_POLL, STAGING};

fn policy(cpus: &[u16]) -> FrequencyPolicy {
    policy_of(0, cpus)
}

fn policy_of(id: u32, cpus: &[u16]) -> FrequencyPolicy {
    FrequencyPolicy {
        id: FrequencyPolicyId::new(id),
        related_cpus: cpus.iter().copied().map(CpuId::from).collect(),
        cpuinfo_min_khz: 400_000,
        scaling_min_khz: 400_000,
        scaling_max_khz: 5_700_000,
        available_khz: None,
    }
}

/// `Limits` driven to completion on a paused-clock runtime, with a token nothing cancels.
struct Blocking(Limits);

impl Blocking {
    fn new(machine: Fake, dir: std::path::PathBuf, fs: Option<Arc<MemFs>>) -> Self {
        let fs = fs.map(|fs| -> Arc<dyn togi_journal::fs::Fs> { fs });
        Self(Limits::new(Arc::new(machine), dir, fs))
    }

    fn cap(
        &self,
        controller: &[CpuId],
        workers: &[CpuId],
        report: &mut Vec<String>,
    ) -> Result<(), Error> {
        crate::testing::runtime(true).block_on(self.0.cap(
            controller,
            workers,
            &CancellationToken::new(),
            report,
        ))
    }

    fn restore(&self, report: &mut Vec<String>) -> Result<(), Error> {
        crate::testing::runtime(true).block_on(self.0.restore(&CancellationToken::new(), report))
    }
}

fn fixture(policies: Vec<FrequencyPolicy>) -> (Fake, Arc<MemFs>, Blocking) {
    let fs = Arc::new(MemFs::new());
    fs.create_dir(Path::new("/trials")).unwrap();
    let machine = Fake::new(policies);
    let limits = Blocking::new(machine.clone(), "/trials".into(), Some(fs.clone()));
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
    let replacement = Blocking::new(machine.clone(), "/trials".into(), Some(fs));
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

fn two_policies() -> Vec<FrequencyPolicy> {
    vec![policy_of(0, &[0, 16]), policy_of(1, &[1, 17])]
}

fn both() -> [CpuId; 2] {
    [CpuId::from(0), CpuId::from(1)]
}

fn pending_path() -> std::path::PathBuf {
    Path::new("/trials").join(PENDING)
}

#[test]
fn failed_restored_persistence_still_restores_every_policy_and_keeps_the_intent() {
    let (machine, fs, limits) = fixture(two_policies());
    limits.cap(&both(), &[], &mut Vec::new()).unwrap();
    fs.inject(Fault::before(Op::Sync, 1));
    let error = limits.restore(&mut Vec::new()).unwrap_err();
    let chain = error_chain(&error);
    assert!(chain.contains("1 of 2 saved policies"), "{chain}");
    assert_eq!(
        machine.writes()[2..],
        [
            (FrequencyPolicyId::new(0), 5_700_000),
            (FrequencyPolicyId::new(1), 5_700_000)
        ]
    );
    assert!(fs.contents(pending_path()).is_some());
    limits.restore(&mut Vec::new()).unwrap();
    assert_eq!(fs.contents(pending_path()), None);
}

#[test_case(false, "saved policy is missing"; "missing policy")]
#[test_case(true, "are now"; "changed related cpus")]
fn recovery_refuses_a_changed_policy_without_writing_it_and_restores_the_rest(
    changed: bool,
    message: &str,
) {
    let (machine, fs, limits) = fixture(two_policies());
    limits.cap(&both(), &[], &mut Vec::new()).unwrap();
    let mut current = machine.policies().unwrap();
    if changed {
        current[0].related_cpus = vec![CpuId::from(0), CpuId::from(2)];
    } else {
        current.remove(0);
    }
    machine.set_policies(current);
    let mut report = Vec::new();
    let error = limits.restore(&mut report).unwrap_err();
    let chain = error_chain(&error);
    assert!(chain.contains(message), "{chain}");
    assert!(chain.contains("1 of 2 saved policies"), "{chain}");
    assert!(chain.contains("first policy0"), "{chain}");
    assert_eq!(
        machine.writes(),
        vec![
            (FrequencyPolicyId::new(0), 400_000),
            (FrequencyPolicyId::new(1), 400_000),
            (FrequencyPolicyId::new(1), 5_700_000)
        ]
    );
    assert!(fs.contents(pending_path()).is_some());
    assert!(
        report
            .iter()
            .any(|line| line.contains("restore failed: policy0"))
    );
}

fn published(fs: &MemFs) -> Value {
    serde_json::from_slice(&fs.contents(pending_path()).unwrap()).unwrap()
}

fn as_positional_pending(value: &Value) -> Value {
    json!([value["boot"], value["policies"]])
}

fn as_positional_saved(value: &Value) -> Value {
    let policies: Vec<Value> = value["policies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|saved| {
            json!([
                saved["policy"],
                saved["related_cpus"],
                saved["original_khz"],
                saved["cap_khz"],
                saved["state"]
            ])
        })
        .collect();
    json!({ "boot": value["boot"], "policies": policies })
}

#[test_case(as_positional_pending; "pending")]
#[test_case(as_positional_saved; "saved")]
fn a_positional_recovery_intent_is_refused_without_any_write(positional: fn(&Value) -> Value) {
    let (machine, fs, limits) = fixture(two_policies());
    limits.cap(&both(), &[], &mut Vec::new()).unwrap();
    let writes = machine.writes();
    let bytes = serde_json::to_vec(&positional(&published(&fs))).unwrap();
    fs.put(pending_path(), bytes.clone());
    assert!(limits.restore(&mut Vec::new()).is_err());
    assert!(
        limits.cap(&both(), &[], &mut Vec::new()).is_err(),
        "an undecodable intent must keep blocking a new transaction"
    );
    assert_eq!(machine.writes(), writes);
    assert_eq!(fs.contents(pending_path()), Some(bytes));
}

#[test_case(false; "regular file")]
#[test_case(true; "symlink")]
fn real_fs_intent_is_private_and_replaces_unsafe_staging_entries(link: bool) {
    let tmp = crate::testing::tempdir();
    let dir = tmp.path().to_owned();
    let victim = dir.join("victim");
    std::fs::write(&victim, "keep").unwrap();
    let staging = dir.join(STAGING);
    if link {
        symlink(&victim, &staging).unwrap();
    } else {
        std::fs::write(&staging, "old").unwrap();
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o666)).unwrap();
    }
    let limits = Blocking::new(Fake::new(two_policies()), dir.clone(), None);
    limits.cap(&both(), &[], &mut Vec::new()).unwrap();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    let intent = std::fs::metadata(dir.join(PENDING)).unwrap();
    assert!(intent.is_file());
    assert_eq!(intent.permissions().mode() & 0o777, 0o600);
    assert!(!staging.exists());
    limits.restore(&mut Vec::new()).unwrap();
}

/// How many reads fit in one deadline after the first: the last read that can still confirm.
fn last_read_in_deadline() -> u32 {
    u32::try_from(READBACK_DEADLINE.as_millis() / READBACK_POLL.as_millis()).unwrap()
}

/// Runs `future` on a paused clock and returns how much virtual time it took.
fn timed<T>(future: impl Future<Output = T>) -> (T, Duration) {
    crate::testing::runtime(true).block_on(async {
        let begun = tokio::time::Instant::now();
        let value = future.await;
        (value, begun.elapsed())
    })
}

#[test_case(1; "second read")]
#[test_case(last_read_in_deadline(); "last read inside the deadline")]
fn a_cap_that_applies_after_some_reads_is_confirmed_within_the_deadline(reads: u32) {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    machine.apply(Apply::AfterReads(reads));
    let (outcome, waited) = timed(limits.0.cap(
        &[CpuId::from(0)],
        &[],
        &CancellationToken::new(),
        &mut Vec::new(),
    ));
    outcome.unwrap();
    assert!(waited <= READBACK_DEADLINE, "{waited:?}");
    assert_eq!(
        machine.scaling_max_khz(FrequencyPolicyId::new(0)).unwrap(),
        400_000
    );
    assert!(fs.contents(pending_path()).is_some());
}

#[test_case(1; "second read")]
#[test_case(last_read_in_deadline(); "last read inside the deadline")]
fn a_restore_that_applies_after_some_reads_is_confirmed_within_the_deadline(reads: u32) {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    limits.cap(&[CpuId::from(0)], &[], &mut Vec::new()).unwrap();
    machine.apply(Apply::AfterReads(reads));
    let (outcome, waited) = timed(limits.0.restore(&CancellationToken::new(), &mut Vec::new()));
    outcome.unwrap();
    assert!(waited <= READBACK_DEADLINE, "{waited:?}");
    assert_eq!(
        machine.scaling_max_khz(FrequencyPolicyId::new(0)).unwrap(),
        5_700_000
    );
    assert_eq!(fs.contents(pending_path()), None);
}

#[test]
fn a_slow_policy_does_not_use_up_the_window_of_a_later_cap() {
    let (machine, _, limits) = fixture(two_policies());
    machine.apply_to(
        FrequencyPolicyId::new(0),
        Apply::AfterReads(last_read_in_deadline()),
    );
    machine.apply_to(FrequencyPolicyId::new(1), Apply::AfterReads(2));
    let (outcome, waited) = timed(limits.0.cap(
        &both(),
        &[],
        &CancellationToken::new(),
        &mut Vec::new(),
    ));
    outcome.unwrap();
    assert!(waited <= READBACK_DEADLINE, "{waited:?}");
    for id in [0, 1] {
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(id)).unwrap(),
            400_000
        );
    }
}

#[test]
fn a_slow_policy_does_not_use_up_the_window_of_a_later_restore() {
    let (machine, fs, limits) = fixture(two_policies());
    limits.cap(&both(), &[], &mut Vec::new()).unwrap();
    machine.apply_to(
        FrequencyPolicyId::new(0),
        Apply::AfterReads(last_read_in_deadline()),
    );
    machine.apply_to(FrequencyPolicyId::new(1), Apply::AfterReads(2));
    let (outcome, waited) = timed(limits.0.restore(&CancellationToken::new(), &mut Vec::new()));
    outcome.unwrap();
    assert!(waited <= READBACK_DEADLINE, "{waited:?}");
    for id in [0, 1] {
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(id)).unwrap(),
            5_700_000
        );
    }
    assert_eq!(fs.contents(pending_path()), None);
}

#[test]
fn a_cap_one_read_beyond_the_deadline_is_unconfirmed_and_recovered() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    machine.apply(Apply::AfterReads(last_read_in_deadline() + 1));
    let mut report = Vec::new();
    let (outcome, waited) = timed(limits.0.cap(
        &[CpuId::from(0)],
        &[],
        &CancellationToken::new(),
        &mut report,
    ));
    let chain = error_chain(&outcome.unwrap_err());
    assert!(
        chain.contains("policy0 cap readback 5700000 kHz differs from 400000 kHz"),
        "{chain}"
    );
    assert_eq!(waited, READBACK_DEADLINE);
    assert!(
        report
            .iter()
            .any(|line| line.contains("cap readback: policy0 5700000 kHz (requested 400000 kHz)")),
        "{report:?}"
    );
    assert!(fs.contents(pending_path()).is_some());
    machine.apply(Apply::Immediately);
    limits.restore(&mut Vec::new()).unwrap();
    assert_eq!(
        machine.scaling_max_khz(FrequencyPolicyId::new(0)).unwrap(),
        5_700_000
    );
    assert_eq!(fs.contents(pending_path()), None);
}

#[test]
fn a_cap_that_never_applies_is_unconfirmed_after_the_deadline_and_recovered() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    machine.apply(Apply::Never);
    let (outcome, waited) = timed(limits.0.cap(
        &[CpuId::from(0)],
        &[],
        &CancellationToken::new(),
        &mut Vec::new(),
    ));
    let chain = error_chain(&outcome.unwrap_err());
    assert!(chain.contains("readback deadline passed"), "{chain}");
    assert_eq!(waited, READBACK_DEADLINE);
    assert!(fs.contents(pending_path()).is_some());
    machine.apply(Apply::Immediately);
    limits.restore(&mut Vec::new()).unwrap();
    assert_eq!(
        machine.scaling_max_khz(FrequencyPolicyId::new(0)).unwrap(),
        5_700_000
    );
    assert_eq!(fs.contents(pending_path()), None);
}

#[test]
fn a_restore_that_never_applies_shares_one_deadline_and_is_retried() {
    let (machine, fs, limits) = fixture(two_policies());
    limits.cap(&both(), &[], &mut Vec::new()).unwrap();
    machine.apply(Apply::Never);
    let (outcome, waited) = timed(limits.0.restore(&CancellationToken::new(), &mut Vec::new()));
    let chain = error_chain(&outcome.unwrap_err());
    assert!(chain.contains("2 of 2 saved policies"), "{chain}");
    assert!(chain.contains("readback deadline passed"), "{chain}");
    assert_eq!(waited, READBACK_DEADLINE);
    assert!(fs.contents(pending_path()).is_some());
    machine.apply(Apply::Immediately);
    limits.restore(&mut Vec::new()).unwrap();
    for id in [0, 1] {
        assert_eq!(
            machine.scaling_max_khz(FrequencyPolicyId::new(id)).unwrap(),
            5_700_000
        );
    }
    assert_eq!(fs.contents(pending_path()), None);
}

#[test]
fn cancellation_ends_a_cap_readback_wait_without_waiting_for_the_next_poll() {
    let (machine, fs, limits) = fixture(vec![policy(&[0, 16])]);
    machine.apply(Apply::Never);
    let cancel = CancellationToken::new();
    let cancelled_at = Duration::from_millis(25);
    let (outcome, waited) = timed(async {
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(cancelled_at).await;
            trigger.cancel();
        });
        limits
            .0
            .cap(&[CpuId::from(0)], &[], &cancel, &mut Vec::new())
            .await
    });
    let chain = error_chain(&outcome.unwrap_err());
    assert!(chain.contains("cancellation ended the wait"), "{chain}");
    assert_eq!(waited, cancelled_at);
    assert!(fs.contents(pending_path()).is_some());
}

#[test]
fn a_cancelled_restore_still_writes_and_reads_every_policy_without_waiting() {
    let (machine, fs, limits) = fixture(two_policies());
    limits.cap(&both(), &[], &mut Vec::new()).unwrap();
    machine.apply(Apply::Never);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (outcome, waited) = timed(limits.0.restore(&cancel, &mut Vec::new()));
    let chain = error_chain(&outcome.unwrap_err());
    assert!(chain.contains("2 of 2 saved policies"), "{chain}");
    assert!(chain.contains("cancellation ended the wait"), "{chain}");
    assert_eq!(waited, Duration::ZERO);
    assert_eq!(machine.writes()[2..].len(), 2);
    assert!(fs.contents(pending_path()).is_some());
}
