use std::sync::Arc;

use togi_machine::{
    CancellationToken, Error, Host, Partial, PmTable, PmTableReader, ReadSamples, Running, Smu,
    Stopped, Swept, TrialConditions, TrialSpec, Trials,
};

use super::support::{Scope, Tree, Watch, host, refused_account};
use crate::adapters::{PmSlot, RunnerHandle, SharedSmu};
use crate::host::Privilege;

struct FixedTable;

fn table() -> PmTable {
    PmTable {
        power_w: [7.0; 16],
        ..PmTable::default()
    }
}

impl PmTableReader for FixedTable {
    fn pm_table(&self) -> Option<PmTable> {
        Some(table())
    }
}

#[test]
fn the_runners_slot_clone_sees_no_table_until_the_reader_is_published() {
    let slot = PmSlot::default();
    let runners = slot.clone();
    assert_eq!(runners.pm_table(), None);
    slot.publish(Arc::new(FixedTable));
    assert_eq!(runners.pm_table(), Some(table()));
}

struct Samples(&'static str);

impl Trials for Samples {
    fn controller_pid(&self) -> u32 {
        123
    }

    fn content(&mut self, _: togi_machine::WorkloadId) -> Result<togi_machine::ContentId, Error> {
        panic!("sample reads must not ask for workload content")
    }

    fn sweep(&mut self, _cancel: &CancellationToken) -> Result<Swept, Error> {
        panic!("sample reads must not sweep workloads")
    }

    fn start(
        &mut self,
        _cancel: &CancellationToken,
        _spec: TrialSpec,
        _ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        panic!("sample reads must not start workloads")
    }

    fn ended(&mut self, _id: &str) -> Result<(), Error> {
        panic!("sample reads must not end trials")
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = Result<TrialConditions, togi_machine::SampleError>> + 'a> {
        assert_eq!(id, "t1");
        Box::new(ReadSamples::from_reader(self.0.as_bytes()))
    }
}

fn trial_with_samples(lines: &'static str) -> RunnerHandle<Samples> {
    RunnerHandle::new(Samples(lines))
}

#[test]
fn the_runner_handle_forwards_its_controller_pid() {
    let handle = trial_with_samples("");
    assert_eq!(handle.controller_pid(), 123);
    assert!(handle.is_unlocked());
}

#[test]
fn samples_read_through_the_handle_keep_the_files_order_and_values() {
    let handle = trial_with_samples(
        "{\"elapsed_ms\":30}\n{\"elapsed_ms\":10,\"tctl_c\":61}\n{\"elapsed_ms\":20}\n",
    );
    let samples: Vec<_> = handle
        .samples("t1")
        .map(Result::unwrap)
        .map(|sample| (sample.elapsed_ms, sample.tctl_c))
        .collect();
    assert_eq!(samples, [(30, None), (10, Some(61)), (20, None)]);
}

#[test]
fn a_live_samples_iterator_does_not_hold_the_runner_lock() {
    let handle = trial_with_samples("{\"elapsed_ms\":1}\n");
    let clone = handle.clone();
    let mut samples = handle.samples("t1");
    assert!(clone.is_unlocked());
    assert_eq!(
        samples.next().map(|sample| sample.unwrap().elapsed_ms),
        Some(1)
    );
    assert!(clone.is_unlocked());
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "proves the shared emergency zeroing refuses before any SMU command"
)]
fn an_emergency_zero_without_mapped_slots_is_refused_before_any_mailbox_command() {
    let tree = Tree::new();
    let mut host = host(&tree, Privilege::Root, refused_account(), Scope::creating());
    let validated = host.validate_smu().unwrap();
    let watch = Watch::on(&tree);
    let mut smu = SharedSmu(Arc::clone(&host.driver));
    let error = smu
        .emergency_zero(validated, &Stopped::confirmed())
        .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("offsets unchanged: the core slots are not mapped: "),
        "{error}"
    );
    // Validation starts the PM table reader, which may read `pm_table_version`/`pm_table`
    // concurrently; only the mailbox and SMN files show an SMU command was sent.
    let commanded: Vec<_> = watch
        .touched()
        .into_iter()
        .filter(|name| !name.starts_with("pm_table"))
        .collect();
    assert_eq!(commanded, Vec::<&str>::new());
}
