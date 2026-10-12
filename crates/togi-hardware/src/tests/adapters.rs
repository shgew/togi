use std::sync::Arc;

use tempfile::TempDir;
use togi_machine::{PmTable, PmTableReader, Trials};
use togi_trial::{Backends, Options, Runner};

use crate::adapters::{PmSlot, RunnerHandle};

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

fn trial_with_samples(state: &TempDir, lines: &str) -> RunnerHandle {
    let trials = state.path().join("trials");
    std::fs::create_dir_all(trials.join("t1")).unwrap();
    std::fs::write(trials.join("t1/samples.jsonl"), lines).unwrap();
    let runner = Runner::new(Options {
        dir: trials,
        backends: Backends::new(),
        topology: Vec::new(),
        identity: None,
        pm_table: None,
    })
    .unwrap();
    RunnerHandle::new(runner)
}

#[test]
fn samples_read_through_the_handle_keep_the_files_order_and_values() {
    let state = TempDir::new().unwrap();
    let handle = trial_with_samples(
        &state,
        "{\"elapsed_ms\":30}\n{\"elapsed_ms\":10,\"tctl_c\":61}\n{\"elapsed_ms\":20}\n",
    );
    let samples: Vec<_> = handle
        .samples("t1")
        .map(|sample| (sample.elapsed_ms, sample.tctl_c))
        .collect();
    assert_eq!(samples, [(30, None), (10, Some(61)), (20, None)]);
}

#[test]
fn a_live_samples_iterator_does_not_hold_the_runner_lock() {
    let state = TempDir::new().unwrap();
    let handle = trial_with_samples(&state, "{\"elapsed_ms\":1}\n");
    let clone = handle.clone();
    let mut samples = handle.samples("t1");
    assert!(clone.is_unlocked());
    assert_eq!(samples.next().map(|sample| sample.elapsed_ms), Some(1));
    assert!(clone.is_unlocked());
}
