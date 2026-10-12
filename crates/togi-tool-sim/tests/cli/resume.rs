use std::fs;
use std::path::Path;

use crate::common::sim;

fn run(dir: &Path, extra: &[&str]) -> Option<i32> {
    let mut args = vec!["--state-dir", dir.to_str().unwrap()];
    args.extend_from_slice(extra);
    let ran = sim(&args);
    ran.code
}

/// A session split across processes by the boot cap ends exactly where the same session run in one process does:
/// the same journal, byte for byte, and the same state document. The cap always falls on a crash, so this holds
/// only when the crash's evidence and the reboot's clock survive the process.
#[test]
fn a_session_resumed_in_a_new_process_ends_where_an_uninterrupted_one_does() {
    let root = tempfile::tempdir().unwrap();
    let whole = root.path().join("whole");
    let split = root.path().join("split");
    assert_eq!(run(&whole, &[]), Some(0));
    let mut resumes = 0;
    let mut code = run(&split, &["--max-boots", "4"]);
    while code == Some(3) {
        resumes += 1;
        assert!(resumes < 20, "the split session never finished");
        code = run(&split, &["--max-boots", "4"]);
    }
    assert_eq!(code, Some(0));
    assert!(resumes >= 2, "the session crashed too rarely to split it");
    assert!(!split.join("archive").exists());
    same(&whole, &split, "events.jsonl");
    same(&whole, &split, "state.json");
}

fn same(whole: &Path, split: &Path, name: &str) {
    let whole = fs::read_to_string(whole.join(name)).unwrap();
    let split = fs::read_to_string(split.join(name)).unwrap();
    if let Some((line, (expected, found))) = whole
        .lines()
        .zip(split.lines())
        .enumerate()
        .find(|(_, (expected, found))| expected != found)
    {
        panic!(
            "{name} differs first at line {}:\nuninterrupted: {expected}\nresumed:       {found}",
            line + 1
        );
    }
    assert_eq!(whole.lines().count(), split.lines().count(), "{name} lines");
}
