use std::error::Error as _;
use std::io;
use std::path::Path;

use togi_journal::MemFs;
use togi_journal::fs::{Fault, Op};

use super::{RemoveError, remove_durably};

/// The rendered chain of `error`, after checking that its message names only the step and the OS error is its source.
fn step_and_cause(error: &RemoveError, step: &str) -> String {
    assert_eq!(error.to_string(), step);
    assert!(
        error
            .source()
            .is_some_and(|source| source.downcast_ref::<io::Error>().is_some()),
        "{error:?}"
    );
    togi_render::error_chain(error)
}

const JOURNAL: &str = "/state/events.jsonl";

#[test]
fn a_removed_file_is_gone_and_removal_precedes_directory_sync() {
    let fs = MemFs::new();
    fs.record_history();
    fs.put(JOURNAL, Vec::new());
    remove_durably(Some(&fs), Path::new(JOURNAL)).unwrap();
    assert!(fs.contents(JOURNAL).is_none());
    assert_eq!(
        fs.log(),
        [
            (Op::Remove, Path::new(JOURNAL).to_path_buf()),
            (Op::SyncDir, Path::new("/state").to_path_buf()),
        ]
    );
}

#[test]
fn a_failed_removal_names_the_step_and_the_path_without_syncing() {
    let fs = MemFs::new();
    fs.record_history();
    fs.put(JOURNAL, Vec::new());
    fs.inject(Fault::before(Op::Remove, 1));
    let error = remove_durably(Some(&fs), Path::new(JOURNAL)).unwrap_err();
    let chain = step_and_cause(&error, &format!("remove {JOURNAL}"));
    assert!(chain.starts_with(&format!("remove {JOURNAL}: ")), "{chain}");
    assert!(fs.contents(JOURNAL).is_some());
    assert_eq!(fs.log(), [(Op::Remove, Path::new(JOURNAL).to_path_buf())]);
}

#[test]
fn a_failed_directory_sync_is_reported_after_the_file_was_removed() {
    let fs = MemFs::new();
    fs.record_history();
    fs.put(JOURNAL, Vec::new());
    fs.inject(Fault::before(Op::SyncDir, 1));
    let error = remove_durably(Some(&fs), Path::new(JOURNAL)).unwrap_err();
    let chain = step_and_cause(&error, "sync /state");
    assert!(chain.starts_with("sync /state: "), "{chain}");
    assert!(fs.contents(JOURNAL).is_none());
    assert_eq!(
        fs.log(),
        [
            (Op::Remove, Path::new(JOURNAL).to_path_buf()),
            (Op::SyncDir, Path::new("/state").to_path_buf()),
        ]
    );
}
